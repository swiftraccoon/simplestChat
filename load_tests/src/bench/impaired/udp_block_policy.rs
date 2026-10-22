//! Bounded fixed-rule interpretation, never a shell language. Unrelated rule
//! remainders stay private and opaque; only our finite literal forms are decoded.
//!
//! Formatter source: upstream iptables v1.8.10, commit
//! 8ae55c2a331e932c0aeef8c6c138bf60deb9fd42 (official project's Fuchsia mirror):
//! extensions/libxt_udp.c::udp_save canonicalizes equal endpoints; comment_save
//! delegates to libxtables/xtables.c::xtables_save_string. Legacy print_rule4 and
//! nft-ipv4 save_rule share the detail/match serializers in xshared/nft-shared.
//! Source project: <https://git.netfilter.org/iptables/>.
//! No runtime compatibility or arbitrary nftables ruleset decoding is implied.

use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(super) const LIMIT: u64 = 256 * 1024;

pub(super) fn backend(bytes: &[u8]) -> Result<&'static str> {
    match bytes {
        b"iptables v1.8.10 (legacy)\n" => Ok("legacy"),
        b"iptables v1.8.10 (nf_tables)\n" => Ok("nf_tables"),
        _ => anyhow::bail!("Unsupported UDP filter backend"),
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Table {
    policies: BTreeMap<String, String>,
    chains: BTreeMap<String, Vec<String>>,
}
fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 28
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-+".contains(&byte))
}
impl Table {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() as u64 <= LIMIT, "UDP filter listing limit");
        let text = std::str::from_utf8(bytes)
            .map_err(|_| anyhow::anyhow!("Invalid UDP filter listing"))?;
        ensure!(
            text.ends_with('\n') && !text.contains('\r') && text.lines().count() <= 4096,
            "Invalid UDP filter listing"
        );
        let mut value = Self {
            policies: BTreeMap::new(),
            chains: BTreeMap::new(),
        };
        for line in text.lines() {
            ensure!(
                line.len() <= 4096 && line.bytes().all(|byte| byte >= 32 && byte != 127),
                "Invalid UDP filter listing"
            );
            let (command, rest) = line
                .split_once(' ')
                .ok_or_else(|| anyhow::anyhow!("Invalid UDP filter listing"))?;
            let (chain, rest) = rest
                .split_once(' ')
                .map_or((rest, None), |(chain, rest)| (chain, Some(rest)));
            ensure!(name(chain), "Invalid UDP filter chain");
            match (command, rest) {
                ("-P", Some(policy))
                    if matches!(chain, "INPUT" | "FORWARD" | "OUTPUT")
                        && matches!(policy, "ACCEPT" | "DROP") =>
                {
                    ensure!(
                        value.policies.insert(chain.into(), policy.into()).is_none()
                            && value.chains.insert(chain.into(), Vec::new()).is_none(),
                        "Duplicate UDP filter chain"
                    );
                }
                ("-N", None) => {
                    ensure!(
                        !matches!(chain, "INPUT" | "FORWARD" | "OUTPUT")
                            && value.chains.insert(chain.into(), Vec::new()).is_none(),
                        "Duplicate UDP filter chain"
                    );
                }
                ("-A", Some(rest)) if !rest.is_empty() => {
                    value
                        .chains
                        .get_mut(chain)
                        .ok_or_else(|| anyhow::anyhow!("Undeclared UDP filter chain"))?
                        .push(rest.into());
                }
                _ => anyhow::bail!("Invalid UDP filter listing"),
            }
        }
        ensure!(value.policies.len() == 3, "Incomplete UDP filter listing");
        Ok(value)
    }
    pub fn signature(&self) -> String {
        let mut digest = Sha256::new();
        for (name, policy) in &self.policies {
            frame(&mut digest, name);
            frame(&mut digest, policy);
        }
        for (name, rules) in &self.chains {
            frame(&mut digest, name);
            digest.update((rules.len() as u64).to_be_bytes());
            for rule in rules {
                frame(&mut digest, rule);
            }
        }
        hex::encode(digest.finalize())
    }
    pub fn counts(&self) -> (usize, usize) {
        (self.chains.len(), self.chains.values().map(Vec::len).sum())
    }
}
fn frame(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
}

pub(in super::super) struct Rules {
    pub chain: String,
    nonce: String,
}
impl Rules {
    pub fn new(run_id: &str) -> Result<Self> {
        let id = uuid::Uuid::parse_str(run_id)?;
        ensure!(id.to_string() == run_id, "Invalid UDP filter run identity");
        let nonce = id.simple().to_string();
        Ok(Self {
            chain: format!("SCU_{}", &nonce[..24]),
            nonce,
        })
    }
    pub fn tag(&self, suffix: &str) -> String {
        format!("scudp_{}_{}", self.nonce, suffix)
    }
    fn rule(&self, stage: u8) -> Vec<String> {
        let values = if stage == 4 {
            vec!["-i", "lo", "-p", "udp", "-m", "comment", "--comment"]
        } else {
            vec![
                "-p",
                "udp",
                "-m",
                "udp",
                if stage == 2 { "--dport" } else { "--sport" },
                "41100",
                "-m",
                "comment",
                "--comment",
            ]
        };
        let mut values: Vec<String> = values.into_iter().map(str::to_owned).collect();
        values.push(self.tag(match stage {
            2 => "dport",
            3 => "sport",
            _ => "input",
        }));
        values.extend([
            "-j".into(),
            if stage == 4 {
                self.chain.clone()
            } else {
                "DROP".into()
            },
        ]);
        values
    }
    pub fn mutation(&self, stage: u8, remove: bool) -> Vec<String> {
        if stage == 1 {
            return vec![if remove { "-X" } else { "-N" }.into(), self.chain.clone()];
        }
        let mut values = vec![
            if remove {
                "-D"
            } else if stage == 4 {
                "-I"
            } else {
                "-A"
            }
            .into(),
            if stage == 4 {
                "INPUT".into()
            } else {
                self.chain.clone()
            },
        ];
        if stage == 4 && !remove {
            values.push("1".into());
        }
        values.extend(self.rule(stage));
        values
    }
    pub fn rendering(&self, stage: u8) -> String {
        self.rule(stage).join(" ")
    }
    fn matches(&self, rule: &str, stage: u8) -> bool {
        // Only fixed ASCII owner tags get optional quotes; there is no general
        // string unquoting, expansion or command construction from readback.
        let expected = self.rendering(stage);
        let tag = self.tag(match stage {
            2 => "dport",
            3 => "sport",
            _ => "input",
        });
        [false, true].into_iter().any(|quoted| {
            let expected = if quoted {
                expected.replace(&tag, &format!("\"{tag}\""))
            } else {
                expected.clone()
            };
            [false, true].into_iter().any(|range| {
                let expected = if range {
                    expected.replace(" 41100 ", " 41100:41100 ")
                } else {
                    expected.clone()
                };
                rule == expected || (stage != 4 && rule == expected.replace(" -m udp", ""))
            })
        })
    }
    /// Strip exactly our known prefix and single head jump. Every other rule
    /// remains byte-identical to the private baseline, with per-chain order.
    pub(super) fn project(&self, mut table: Table) -> Result<(Table, u8)> {
        let owned = table.chains.remove(&self.chain);
        let mut stage = if let Some(rules) = owned {
            ensure!(
                rules.len() <= 2
                    && rules
                        .iter()
                        .enumerate()
                        .all(|(index, rule)| self.matches(rule, index as u8 + 2)),
                "Foreign UDP filter chain member"
            );
            1 + rules.len() as u8
        } else {
            0
        };
        for (chain, rules) in &mut table.chains {
            let mut jump = None;
            for (index, rule) in rules.iter().enumerate() {
                if rule.contains(&self.chain) || rule.contains(&self.nonce) {
                    ensure!(
                        chain == "INPUT"
                            && index == 0
                            && stage == 3
                            && jump.is_none()
                            && self.matches(rule, 4),
                        "Foreign UDP filter reference"
                    );
                    jump = Some(index);
                }
            }
            if let Some(index) = jump {
                rules.remove(index);
                stage = 4;
            }
        }
        Ok((table, stage))
    }
}

#[cfg(test)]
#[path = "udp_block_policy_tests.rs"]
mod tests;
