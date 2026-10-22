//! Finite formatter fixtures derived from the pinned upstream save functions.
use super::*;
const ID: &str = "11223344-5566-7788-99aa-bbccddeeff00";
const BASE: &str = "-P INPUT ACCEPT\n-P FORWARD DROP\n-P OUTPUT ACCEPT\n";
fn listing(rules: &Rules, stage: u8) -> String {
    let mut value = BASE.to_owned();
    if stage > 0 {
        value += &format!("-N {}\n", rules.chain);
    }
    if stage == 4 {
        value += &format!("-A INPUT {}\n", rules.rendering(4));
    }
    for index in 2..=stage.min(3) {
        value += &format!("-A {} {}\n", rules.chain, rules.rendering(index));
    }
    value
}
#[test]
fn pinned_formatters_support_only_reviewed_versions_and_literal_variants() {
    for backend_name in ["legacy", "nf_tables"] {
        assert_eq!(
            backend(format!("iptables v1.8.10 ({backend_name})\n").as_bytes()).unwrap(),
            backend_name
        );
    }
    for version in ["1.8.9", "1.8.11", "1.8.12", "1.8.13"] {
        assert!(backend(format!("iptables v{version} (nf_tables)\n").as_bytes()).is_err());
    }
    let rules = Rules::new(ID).unwrap();
    assert_eq!(rules.chain.len(), 28);
    assert!(
        rules
            .tag("input")
            .contains("112233445566778899aabbccddeeff00")
    );
    let baseline = Table::parse(BASE.as_bytes()).unwrap();
    for stage in 0..=4 {
        for quotes in [false, true] {
            for range in [false, true] {
                for explicit_udp in [false, true] {
                    let mut value = listing(&rules, stage);
                    if quotes {
                        for suffix in ["input", "sport", "dport"] {
                            let tag = rules.tag(suffix);
                            value = value.replace(&tag, &format!("\"{tag}\""));
                        }
                    }
                    if range {
                        value = value.replace(" 41100 ", " 41100:41100 ");
                    }
                    if !explicit_udp {
                        value = value.replace(" -m udp", "");
                    }
                    let (stripped, actual) = rules
                        .project(Table::parse(value.as_bytes()).unwrap())
                        .unwrap();
                    assert_eq!(stripped.signature(), baseline.signature());
                    assert_eq!(actual, stage);
                }
            }
        }
    }
}
#[test]
fn private_baseline_preserves_every_unrelated_rule_and_per_chain_order() {
    let rules = Rules::new(ID).unwrap();
    let extra = "-N OTHER\n-A INPUT -m comment --comment \"PRIVATE sentinel $()\" -j ACCEPT\n\
-A OTHER -p tcp --dport 443 -j ACCEPT\n";
    let baseline = Table::parse(format!("{BASE}{extra}").as_bytes()).unwrap();
    let mut active = listing(&rules, 4);
    active = active.replace("-A INPUT", "-N OTHER\n-A INPUT");
    active += &extra[extra.find("-A INPUT").unwrap()..];
    let (stripped, stage) = rules
        .project(Table::parse(active.as_bytes()).unwrap())
        .unwrap();
    assert_eq!(stage, 4);
    assert_eq!(stripped.signature(), baseline.signature());
    let changed = Table::parse(format!("{BASE}{extra}").replace("443", "444").as_bytes()).unwrap();
    assert_ne!(changed.signature(), baseline.signature());
}
#[test]
fn foreign_members_references_and_duplicate_jumps_never_become_cleanup_arguments() {
    let rules = Rules::new(ID).unwrap();
    let good = listing(&rules, 4);
    for bad in [
        format!("{good}-A {} -j ACCEPT\n", rules.chain),
        format!("{good}-A OUTPUT -j {}\n", rules.chain),
        format!("{good}-A INPUT {}\n", rules.rendering(4)),
        good.replace("-i lo", "-i eth0"),
        good.replace("--dport 41100", "--dport 41101"),
        good.replace("-A INPUT", "-A OUTPUT"),
    ] {
        assert!(
            rules
                .project(Table::parse(bad.as_bytes()).unwrap())
                .is_err()
        );
    }
    let args = rules.mutation(4, true);
    assert_eq!(&args[..2], ["-D", "INPUT"]);
    assert!(
        !args
            .iter()
            .any(|arg| matches!(arg.as_str(), "-F" | "--flush"))
    );
}
#[test]
fn listing_bounds_and_declarations_are_fail_closed_with_fixed_errors() {
    for bytes in [
        b"".to_vec(),
        BASE.trim_end().as_bytes().to_vec(),
        format!("{BASE}-P INPUT ACCEPT\n").into_bytes(),
        format!("{BASE}-A UNKNOWN -j ACCEPT\n").into_bytes(),
        format!("{BASE}-A INPUT private\ttext\n").into_bytes(),
        vec![b'x'; LIMIT as usize + 1],
        format!("{BASE}-A INPUT {}\n", "x".repeat(4096)).into_bytes(),
    ] {
        let error = Table::parse(&bytes).err().unwrap();
        assert!(!error.to_string().contains("private"));
    }
}

#[test]
fn upstream_1810_legacy_and_nft_shared_save_functions_have_finite_independent_fixtures() {
    // Literal presentation, independently transcribed from udp_save,
    // comment_save/save_string and print_rule4/nft_ipv4_save_rule. Both
    // backends use the same -m ordering and single equal-endpoint port.
    // libxt_udp.c SHA256 6ff449ddac49c69f2f186e28cb72f053f0
    // 1156dfc7c8dd37b5398182d8effaa5 (Ubuntu Noble and upstream agree).
    let literal = concat!(
        "-P INPUT ACCEPT\n-P FORWARD DROP\n-P OUTPUT ACCEPT\n",
        "-N SCU_112233445566778899aabbcc\n",
        "-A INPUT -i lo -p udp -m comment --comment ",
        "scudp_112233445566778899aabbccddeeff00_input -j SCU_112233445566778899aabbcc\n",
        "-A SCU_112233445566778899aabbcc -p udp -m udp --dport 41100 ",
        "-m comment --comment scudp_112233445566778899aabbccddeeff00_dport -j DROP\n",
        "-A SCU_112233445566778899aabbcc -p udp -m udp --sport 41100 ",
        "-m comment --comment scudp_112233445566778899aabbccddeeff00_sport -j DROP\n",
    );
    let rules = Rules::new(ID).unwrap();
    let baseline = Table::parse(BASE.as_bytes()).unwrap();
    for version in [
        b"iptables v1.8.10 (legacy)\n".as_slice(),
        b"iptables v1.8.10 (nf_tables)\n".as_slice(),
    ] {
        backend(version).unwrap();
        let (stripped, stage) = rules
            .project(Table::parse(literal.as_bytes()).unwrap())
            .unwrap();
        assert_eq!(stage, 4);
        assert_eq!(stripped.signature(), baseline.signature());
    }
}
