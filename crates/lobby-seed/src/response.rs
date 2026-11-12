//! One HTTP/1.x response from the private backend, read within bounds. A
//! body is read only from a successful answer; every answer's cookies count.

/// The most bytes a response's head may take.
const MAX_HEAD: usize = 64 * 1024;
const MAX_HEADERS: usize = 100;

/// Why reading stopped before the next byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    /// The whole operation ran out of time.
    Deadline,
    /// The connection failed or stayed silent too long.
    Broken,
}

/// Where a response's bytes come from. `Ok(0)` is the end.
pub(crate) trait Incoming {
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Stop>;
}

/// Why no answer could be taken from the connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    /// Nothing usable arrived: the backend is absent, silent or gone.
    Unavailable,
    /// What arrived is not a response this client understands.
    Malformed,
    Deadline,
    /// A successful answer carries more than the allowed bytes.
    TooLarge,
}

/// One response: its status, its `Set-Cookie` headers in order, and the body
/// of a successful answer, or why that body could not be taken. The head
/// stands either way: its cookies count even when the body fails.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) cookies: Vec<String>,
    pub(crate) body: Result<Vec<u8>, Fault>,
}

enum Framing {
    Length(usize),
    Chunked,
    Close,
}

struct Head {
    status: u16,
    cookies: Vec<String>,
    framing: Framing,
    length: usize,
}

/// Parse a complete head, or nothing while more bytes are needed.
fn head(bytes: &[u8]) -> Result<Option<Head>, Fault> {
    let mut storage = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut response = httparse::Response::new(&mut storage);
    let length = match response.parse(bytes).map_err(|_| Fault::Malformed)? {
        httparse::Status::Complete(length) => length,
        httparse::Status::Partial => return Ok(None),
    };
    let status = response.code.ok_or(Fault::Malformed)?;
    let (mut cookies, mut chunked, mut announced) = (Vec::new(), false, None);
    for header in response.headers.iter() {
        let value = std::str::from_utf8(header.value).map_err(|_| Fault::Malformed);
        if header.name.eq_ignore_ascii_case("set-cookie") {
            cookies.push(value?.to_owned());
        } else if header.name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value?.trim().eq_ignore_ascii_case("chunked");
        } else if header.name.eq_ignore_ascii_case("content-length") {
            // Every stated length is the same plain number.
            let text = value?.trim();
            let digits = !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
            let stated: usize = text.parse().map_err(|_| Fault::Malformed)?;
            if !digits || announced.is_some_and(|earlier| earlier != stated) {
                return Err(Fault::Malformed);
            }
            announced = Some(stated);
        }
    }
    let framing = match (status, chunked, announced) {
        (204, _, _) => Framing::Length(0),
        (_, true, _) => Framing::Chunked,
        (_, false, Some(stated)) => Framing::Length(stated),
        (_, false, None) => Framing::Close,
    };
    Ok(Some(Head {
        status,
        cookies,
        framing,
        length,
    }))
}

/// Bytes already received and the connection they came from.
struct Stream<'a, I: Incoming> {
    source: &'a mut I,
    pending: Vec<u8>,
}

impl<I: Incoming> Stream<'_, I> {
    /// Receive more, or say that the connection ended.
    fn more(&mut self) -> Result<bool, Stop> {
        let mut piece = [0_u8; 8192];
        let count = self.source.read(&mut piece)?;
        self.pending.extend_from_slice(&piece[..count]);
        Ok(count != 0)
    }

    /// Take up to `count` bytes, fewer only when the connection ends first.
    fn take(&mut self, count: usize) -> Result<Vec<u8>, Stop> {
        while self.pending.len() < count && self.more()? {}
        let taken = count.min(self.pending.len());
        Ok(self.pending.drain(..taken).collect())
    }

    /// Take one line without its ending, or nothing when the connection
    /// ends before the line does.
    fn line(&mut self) -> Result<Option<Vec<u8>>, Stop> {
        loop {
            if let Some(end) = self.pending.windows(2).position(|pair| pair == b"\r\n") {
                let line = self.pending.drain(..end + 2).take(end).collect();
                return Ok(Some(line));
            }
            if self.pending.len() > MAX_HEAD || !self.more()? {
                return Ok(None);
            }
        }
    }
}

/// A failure while a body is read: the answer had begun, so the backend was
/// there.
fn interrupted(stop: Stop) -> Fault {
    match stop {
        Stop::Deadline => Fault::Deadline,
        Stop::Broken => Fault::Malformed,
    }
}

/// The body in chunks, up to one byte more than `most`.
fn chunks<I: Incoming>(stream: &mut Stream<'_, I>, most: usize) -> Result<Vec<u8>, Fault> {
    let mut body = Vec::new();
    while body.len() <= most {
        let line = stream.line().map_err(interrupted)?;
        let line = line.ok_or(Fault::Malformed)?;
        let size = line.split(|byte| *byte == b';').next().unwrap_or_default();
        let size = std::str::from_utf8(size).map_err(|_| Fault::Malformed)?;
        let size = usize::from_str_radix(size.trim(), 16).map_err(|_| Fault::Malformed)?;
        if size == 0 {
            break;
        }
        let wanted = size.min(most + 1 - body.len());
        // A chunk that ends early leaves no line end or next size to find.
        let piece = stream.take(wanted).map_err(interrupted)?;
        body.extend_from_slice(&piece);
        if wanted == size && stream.take(2).map_err(interrupted)? != b"\r\n" {
            return Err(Fault::Malformed);
        }
    }
    Ok(body)
}

/// Read one answer. A successful one may carry at most `most` body bytes; a
/// body shorter than it was said to be is taken as it came.
pub(crate) fn answer<I: Incoming>(source: &mut I, most: usize) -> Result<Answer, Fault> {
    let mut stream = Stream {
        source,
        pending: Vec::new(),
    };
    let head = loop {
        if let Some(head) = head(&stream.pending)? {
            break head;
        }
        let more = stream.more().map_err(|stop| match stop {
            Stop::Deadline => Fault::Deadline,
            Stop::Broken => Fault::Unavailable,
        })?;
        match (more, stream.pending.is_empty()) {
            (true, _) if stream.pending.len() <= MAX_HEAD => {}
            (false, true) => return Err(Fault::Unavailable),
            _ => return Err(Fault::Malformed),
        }
    };
    stream.pending.drain(..head.length);
    let body = match (head.framing, (200..300).contains(&head.status)) {
        (_, false) => Ok(Vec::new()),
        (Framing::Length(stated), true) => {
            let wanted = stated.min(most + 1);
            stream.take(wanted).map_err(interrupted)
        }
        (Framing::Close, true) => stream.take(most + 1).map_err(interrupted),
        (Framing::Chunked, true) => chunks(&mut stream, most),
    };
    let body = body.and_then(|body| match body.len() > most {
        true => Err(Fault::TooLarge),
        false => Ok(body),
    });
    Ok(Answer {
        status: head.status,
        cookies: head.cookies,
        body,
    })
}
