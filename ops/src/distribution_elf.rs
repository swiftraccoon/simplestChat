//! Use only object's low-level ELF readers, with region/count preflight before
//! any ReadCache allocation. No section, symbol, compression or write APIs.

use super::{Architecture, ExecutableIdentity, Failure, FileType};
use crate::files::{CheckedStream, Snapshot};
use object::{
    LittleEndian, elf,
    read::{
        ReadCache, ReadRef, StringTable,
        elf::{Dyn, FileHeader, ProgramHeader},
    },
};
use std::mem::size_of;

const MAX_TABLE: u16 = 8192;
const MAX_STRINGS: u64 = 1024 * 1024;
const MAX_INTERPRETER: u64 = 4096;
// Explicit supported mapping domain: Linux x86-64 with 4 KiB base pages.
// Linux elf_map rounds LOAD mappings to pages, not raw p_vaddr..p_memsz bytes.
const PAGE: u64 = 4096;
type Header = elf::FileHeader64<LittleEndian>;
type Segment = elf::ProgramHeader64<LittleEndian>;
type Cache<'a> = ReadCache<CheckedStream<'a>>;
const ENDIAN: LittleEndian = LittleEndian;

fn region(offset: u64, size: u64, length: u64) -> Result<(), Failure> {
    if offset > length || size > length - offset {
        Err(Failure::Segment)
    } else {
        Ok(())
    }
}

pub(super) fn inspect(
    snapshot: &mut Snapshot,
    sha256: String,
) -> Result<ExecutableIdentity, Failure> {
    let length = snapshot.length();
    let cache = ReadCache::new(snapshot.checked_stream());
    let header = Header::parse(&cache).map_err(|_| Failure::Header)?;
    if !header.is_little_endian()
        || header.e_machine(ENDIAN) != elf::EM_X86_64
        || header.e_version(ENDIAN) != 1
        || header.e_ehsize(ENDIAN) as usize != size_of::<Header>()
        || !matches!(
            header.e_ident().os_abi,
            elf::ELFOSABI_NONE | elf::ELFOSABI_LINUX
        )
    {
        return Err(Failure::Header);
    }
    let file_type = match header.e_type(ENDIAN) {
        elf::ET_EXEC => FileType::Executable,
        elf::ET_DYN => FileType::Dynamic,
        _ => return Err(Failure::Header),
    };
    let count = header.e_phnum(ENDIAN);
    // Refuse PN_XNUM rather than consulting attacker-controlled section zero.
    if !(1..=MAX_TABLE).contains(&count)
        || header.e_phentsize(ENDIAN) as usize != size_of::<Segment>()
        || header.e_phoff(ENDIAN) < size_of::<Header>() as u64
    {
        return Err(Failure::ProgramHeaders);
    }
    region(
        header.e_phoff(ENDIAN),
        u64::from(count) * size_of::<Segment>() as u64,
        length,
    )
    .map_err(|_| Failure::ProgramHeaders)?;
    let segments = header
        .program_headers(ENDIAN, &cache)
        .map_err(|_| Failure::ProgramHeaders)?;
    validate_loads(header, segments, length)?;
    let mut identity = ExecutableIdentity {
        sha256,
        bytes: length,
        architecture: Architecture::X86_64,
        elf_class: 64,
        little_endian: true,
        file_type,
        program_headers: count,
        no_interpreter: true,
        needed_count: 0,
        no_needed: true,
        pie_flag: false,
        bind_now: false,
        non_executable_stack: false,
        no_writable_executable_load: true,
        relro_present: false,
        no_rpath: true,
        no_runpath: true,
        no_text_relocations: true,
        no_loader_hooks: true,
    };
    let mut executable_entry = false;
    let mut stack = false;
    let mut dynamic = None;
    for segment in segments {
        let flags = segment.p_flags(ENDIAN).0;
        match segment.p_type(ENDIAN) {
            elf::PT_LOAD => {
                let start = segment.p_vaddr(ENDIAN);
                let end = start + segment.p_filesz(ENDIAN);
                executable_entry |=
                    flags & elf::PF_X.0 != 0 && (start..end).contains(&header.e_entry(ENDIAN));
                identity.no_writable_executable_load &=
                    flags & (elf::PF_X.0 | elf::PF_W.0) != (elf::PF_X.0 | elf::PF_W.0);
            }
            elf::PT_INTERP => {
                if !identity.no_interpreter {
                    return Err(Failure::Segment);
                }
                identity.no_interpreter = false;
                let size = segment.p_filesz(ENDIAN);
                if !(2..=MAX_INTERPRETER).contains(&size) {
                    return Err(Failure::Segment);
                }
                let bytes = segment.data(ENDIAN, &cache).map_err(|_| Failure::Segment)?;
                if bytes.last() != Some(&0) || bytes[..bytes.len() - 1].contains(&0) {
                    return Err(Failure::Segment);
                }
            }
            elf::PT_DYNAMIC => {
                if dynamic.replace(segment).is_some() {
                    return Err(Failure::Dynamic);
                }
            }
            elf::PT_GNU_STACK => {
                if stack {
                    return Err(Failure::Segment);
                }
                stack = true;
                identity.non_executable_stack = flags & elf::PF_X.0 == 0;
            }
            elf::PT_GNU_RELRO => identity.relro_present |= segment.p_memsz(ENDIAN) != 0,
            _ => {}
        }
    }
    if !executable_entry {
        return Err(Failure::Segment);
    }
    if let Some(segment) = dynamic {
        let mapped = load_region(segments, segment.p_vaddr(ENDIAN), segment.p_filesz(ENDIAN))
            .map_err(|_| Failure::Dynamic)?;
        if mapped != segment.p_offset(ENDIAN) {
            return Err(Failure::Dynamic);
        }
        inspect_dynamic(&cache, segment, segments, &mut identity)?;
    }
    Ok(identity)
}

fn validate_loads(header: &Header, segments: &[Segment], length: u64) -> Result<(), Failure> {
    let mut pages = Vec::new();
    let mut mapped_headers = None;
    let table = header.e_phoff(ENDIAN);
    let table_size = u64::from(header.e_phnum(ENDIAN)) * size_of::<Segment>() as u64;
    let table_end = table
        .checked_add(table_size)
        .ok_or(Failure::ProgramHeaders)?;
    for segment in segments {
        validate_segment(segment, length)?;
        if segment.p_type(ENDIAN) != elf::PT_LOAD || segment.p_memsz(ENDIAN) == 0 {
            continue;
        }
        let start = segment.p_vaddr(ENDIAN);
        let end = start
            .checked_add(segment.p_memsz(ENDIAN))
            .ok_or(Failure::Segment)?;
        let rounded_end = end.checked_add(PAGE - 1).ok_or(Failure::Segment)?;
        pages.push((start - start % PAGE, rounded_end - rounded_end % PAGE));
        let offset = segment.p_offset(ENDIAN);
        let file_end = offset
            .checked_add(segment.p_filesz(ENDIAN))
            .ok_or(Failure::Segment)?;
        // Linux derives AT_PHDR from a LOAD containing e_phoff, not PT_PHDR.
        // Require that whole original table, and its mapping, be unambiguous.
        if offset <= table && table < file_end {
            if table_end > file_end {
                return Err(Failure::ProgramHeaders);
            }
            let address = start
                .checked_add(table - offset)
                .ok_or(Failure::ProgramHeaders)?;
            if mapped_headers.replace(address).is_some() {
                return Err(Failure::ProgramHeaders);
            }
        }
    }
    pages.sort_unstable();
    if pages.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(Failure::Segment);
    }
    let mapped_headers = mapped_headers.ok_or(Failure::ProgramHeaders)?;
    let mut seen = false;
    for segment in segments {
        if segment.p_type(ENDIAN) == elf::PT_PHDR {
            if seen
                || segment.p_offset(ENDIAN) != table
                || segment.p_vaddr(ENDIAN) != mapped_headers
                || segment.p_filesz(ENDIAN) != table_size
                || segment.p_memsz(ENDIAN) != table_size
            {
                return Err(Failure::ProgramHeaders);
            }
            seen = true;
        }
    }
    Ok(())
}

fn validate_segment(segment: &Segment, length: u64) -> Result<(), Failure> {
    let size = segment.p_filesz(ENDIAN);
    let memory = segment.p_memsz(ENDIAN);
    let address = segment.p_vaddr(ENDIAN);
    region(segment.p_offset(ENDIAN), size, length)?;
    if size > memory || address.checked_add(memory).is_none() {
        return Err(Failure::Segment);
    }
    if segment.p_type(ENDIAN) == elf::PT_LOAD && address % PAGE != segment.p_offset(ENDIAN) % PAGE {
        return Err(Failure::Segment);
    }
    let alignment = segment.p_align(ENDIAN);
    if alignment > 1
        && (!alignment.is_power_of_two()
            || (segment.p_type(ENDIAN) == elf::PT_LOAD
                && address % alignment != segment.p_offset(ENDIAN) % alignment))
    {
        return Err(Failure::Segment);
    }
    Ok(())
}

fn inspect_dynamic(
    cache: &Cache<'_>,
    segment: &Segment,
    segments: &[Segment],
    identity: &mut ExecutableIdentity,
) -> Result<(), Failure> {
    let size = segment.p_filesz(ENDIAN);
    let entry_size = size_of::<elf::Dyn64<LittleEndian>>() as u64;
    if size == 0 || !size.is_multiple_of(entry_size) || size / entry_size > u64::from(MAX_TABLE) {
        return Err(Failure::Dynamic);
    }
    let entries = segment
        .dynamic(ENDIAN, cache)
        .map_err(|_| Failure::Dynamic)?
        .ok_or(Failure::Dynamic)?;
    let mut strings = None;
    let mut strings_size = None;
    let mut flags = None;
    let mut flags1 = None;
    let mut terminated = false;
    let mut string_offsets = Vec::new();
    for entry in entries {
        let tag = entry.d_tag(ENDIAN);
        let value = entry.d_val(ENDIAN);
        if terminated {
            if tag != elf::DT_NULL || value != 0 {
                return Err(Failure::Dynamic);
            }
            continue;
        }
        match tag {
            elf::DT_NULL => terminated = true,
            elf::DT_STRTAB => unique(&mut strings, value)?,
            elf::DT_STRSZ => unique(&mut strings_size, value)?,
            elf::DT_FLAGS => unique(&mut flags, value)?,
            elf::DT_FLAGS_1 => unique(&mut flags1, value)?,
            elf::DT_NEEDED => {
                identity.needed_count += 1;
                string_offsets.push(value);
            }
            elf::DT_RPATH => {
                identity.no_rpath = false;
                string_offsets.push(value);
            }
            elf::DT_RUNPATH => {
                identity.no_runpath = false;
                string_offsets.push(value);
            }
            elf::DT_SONAME => string_offsets.push(value),
            elf::DT_BIND_NOW => identity.bind_now = true,
            elf::DT_TEXTREL => identity.no_text_relocations = false,
            elf::DT_AUDIT | elf::DT_DEPAUDIT | elf::DT_AUXILIARY | elf::DT_FILTER => {
                identity.no_loader_hooks = false;
                string_offsets.push(value);
            }
            _ => {}
        }
    }
    if !terminated {
        return Err(Failure::Dynamic);
    }
    identity.no_needed = identity.needed_count == 0;
    let flags = flags.unwrap_or(0);
    let flags1 = flags1.unwrap_or(0);
    identity.pie_flag = flags1 & elf::DF_1_PIE.0 != 0;
    identity.bind_now |= flags & elf::DF_BIND_NOW.0 != 0 || flags1 & elf::DF_1_NOW.0 != 0;
    identity.no_text_relocations &= flags & elf::DF_TEXTREL.0 == 0;
    match (strings, strings_size) {
        (None, None) if string_offsets.is_empty() => Ok(()),
        (Some(address), Some(size)) if (1..=MAX_STRINGS).contains(&size) => {
            let offset = load_region(segments, address, size)?;
            let bytes = cache
                .read_bytes_at(offset, size)
                .map_err(|_| Failure::Strings)?;
            let table = StringTable::new(bytes, 0, size);
            for offset in string_offsets {
                let offset = u32::try_from(offset).map_err(|_| Failure::Strings)?;
                let value = table.get(offset).map_err(|_| Failure::Strings)?;
                if value.is_empty() || value.len() > 4096 {
                    return Err(Failure::Strings);
                }
            }
            Ok(())
        }
        _ => Err(Failure::Strings),
    }
}

fn unique(slot: &mut Option<u64>, value: u64) -> Result<(), Failure> {
    if slot.replace(value).is_some() {
        Err(Failure::Dynamic)
    } else {
        Ok(())
    }
}

fn load_region(segments: &[Segment], address: u64, size: u64) -> Result<u64, Failure> {
    let end = address.checked_add(size).ok_or(Failure::Strings)?;
    let mut offset = None;
    for segment in segments {
        if segment.p_type(ENDIAN) != elf::PT_LOAD {
            continue;
        }
        let start = segment.p_vaddr(ENDIAN);
        if segment.p_memsz(ENDIAN) == 0 {
            continue;
        }
        let memory_end = start
            .checked_add(segment.p_memsz(ENDIAN))
            .ok_or(Failure::Strings)?;
        let file_end = start
            .checked_add(segment.p_filesz(ENDIAN))
            .ok_or(Failure::Strings)?;
        let page_start = start - start % PAGE;
        let page_end = memory_end
            .checked_add(PAGE - 1)
            .map(|end| end - end % PAGE)
            .ok_or(Failure::Strings)?;
        // A later LOAD can replace a whole base page even when its raw byte
        // range misses the metadata. Include rounded mappings and BSS tails;
        // refuse every extra intersecting page span rather than model a loader.
        if page_start < end && address < page_end {
            if start > address || end > file_end {
                return Err(Failure::Strings);
            }
            let candidate = segment
                .p_offset(ENDIAN)
                .checked_add(address - start)
                .ok_or(Failure::Strings)?;
            if offset.replace(candidate).is_some() {
                return Err(Failure::Strings);
            }
        }
    }
    offset.ok_or(Failure::Strings)
}
