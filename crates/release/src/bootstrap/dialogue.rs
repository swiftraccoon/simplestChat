//! The bounded English OpenSSH and Debian password exchange: which prompt is
//! answered with which secret, and in which order. The secrets themselves
//! never enter this module.

use super::{Result, require};
use regex::Regex;
use std::sync::LazyLock;

/// What the remote enrollment command prints once the key is in place.
pub(super) const ENROLLED: &str = "SIMPLESTCHAT_BOOTSTRAP_KEY_ENROLLED";
/// A dialogue never answers more prompts than this.
const LIMIT: usize = 5;

/// One recognised password prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Prompt {
    Login,
    Current,
    New,
    Confirm,
}

/// Which of the two secrets answers a prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Secret {
    Current,
    Replacement,
}

/// Prompts in the order they are tried; a confirmation also reads as a new
/// password prompt, and every prompt reads as a login prompt.
static PROMPTS: LazyLock<[(Prompt, Regex); 4]> = LazyLock::new(|| {
    let pattern = |text: &str| Regex::new(&format!("(?i){text}")).unwrap();
    [
        (
            Prompt::Confirm,
            pattern(r"(?:retype|repeat) (?:new )?(?:unix )?password:\s*$"),
        ),
        (
            Prompt::New,
            pattern(r"(?:enter )?new (?:unix )?password:\s*$"),
        ),
        (
            Prompt::Current,
            pattern(r"(?:\(current\)|current|old) (?:unix )?password:\s*$"),
        ),
        (Prompt::Login, pattern(r"(?:^|[\r\n])[^\r\n]*password:\s*$")),
    ]
});

/// The prompts answered so far and what the remote side has reported.
#[derive(Debug, Default)]
pub(super) struct Dialogue {
    seen: Vec<Prompt>,
    pub rotated: bool,
    pub enrolled: bool,
}

impl Dialogue {
    /// A dialogue whose login prompt was already answered out of band.
    pub fn after_login() -> Self {
        Self {
            seen: vec![Prompt::Login],
            ..Self::default()
        }
    }

    /// Reads the text pending since the last answer. A recognised prompt is
    /// consumed once; a repeated or reordered prompt fails closed.
    pub fn consume(&mut self, text: &str) -> Result<Option<Secret>> {
        if text.contains(ENROLLED) {
            self.enrolled = true;
        }
        if text
            .to_lowercase()
            .contains("password updated successfully")
        {
            let confirmed = self.seen.contains(&Prompt::Confirm);
            require(confirmed, "unexpected_password_success")?;
            self.rotated = true;
        }
        let Some((prompt, _)) = PROMPTS.iter().find(|(_, pattern)| pattern.is_match(text)) else {
            return Ok(None);
        };
        let seen = |prompt: Prompt| self.seen.contains(&prompt);
        require(!seen(*prompt), "repeated_password_prompt")?;
        require(self.seen.len() < LIMIT, "password_prompt_limit")?;
        let ordered = match prompt {
            Prompt::New => seen(Prompt::Login) || seen(Prompt::Current),
            Prompt::Confirm => seen(Prompt::New),
            Prompt::Current | Prompt::Login => !seen(Prompt::New),
        };
        require(ordered, "password_prompt_order")?;
        self.seen.push(*prompt);
        Ok(Some(match prompt {
            Prompt::New | Prompt::Confirm => Secret::Replacement,
            Prompt::Current | Prompt::Login => Secret::Current,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::super::Refusal;
    use super::*;

    fn refused(result: Result<Option<Secret>>) -> &'static str {
        let Refusal(code) = result.unwrap_err();
        code
    }

    /// From `test_forced_change_prompts_are_ordered_and_not_in_repr`.
    #[test]
    fn forced_change_prompts_are_ordered() {
        let mut dialogue = Dialogue::default();
        let login = dialogue.consume("debian@host's password: ").unwrap();
        assert_eq!(login, Some(Secret::Current));
        let current = dialogue.consume("Current password: ").unwrap();
        assert_eq!(current, Some(Secret::Current));
        let new = dialogue.consume("New password: ").unwrap();
        assert_eq!(new, Some(Secret::Replacement));
        let confirm = dialogue.consume("Retype new password: ").unwrap();
        assert_eq!(confirm, Some(Secret::Replacement));
        let done = dialogue.consume("passwd: password updated successfully\n");
        assert_eq!(done.unwrap(), None);
        assert!(dialogue.rotated && !dialogue.enrolled);
    }

    /// From `test_repeated_and_reordered_prompts_fail_without_echoing_input`.
    #[test]
    fn repeated_and_reordered_prompts_fail_closed() {
        let mut dialogue = Dialogue::default();
        dialogue.consume("password: ").unwrap();
        assert_eq!(
            refused(dialogue.consume("password: ")),
            "repeated_password_prompt"
        );
        let mut fresh = Dialogue::default();
        assert_eq!(
            refused(fresh.consume("Retype new password: ")),
            "password_prompt_order"
        );
        let mut early = Dialogue::default();
        assert_eq!(
            refused(early.consume("New password: ")),
            "password_prompt_order"
        );
        let mut late = Dialogue::after_login();
        late.consume("New UNIX password: ").unwrap();
        assert_eq!(
            refused(late.consume("(current) UNIX password: ")),
            "password_prompt_order"
        );
    }

    /// A login answered through the askpass helper is not answered again on
    /// the session's stream, and the forced change follows it in order.
    #[test]
    fn a_dialogue_after_login_refuses_a_second_login_prompt() {
        let mut dialogue = Dialogue::after_login();
        assert_eq!(
            refused(dialogue.consume("debian@host's password: ")),
            "repeated_password_prompt"
        );
        let mut forced = Dialogue::after_login();
        let banner = "You must change your password now and login again!\r\n";
        assert_eq!(forced.consume(banner).unwrap(), None);
        let current = forced.consume("Changing password for debian.\r\nCurrent password: ");
        assert_eq!(current.unwrap(), Some(Secret::Current));
        assert_eq!(
            forced.consume("\r\nNew password: ").unwrap(),
            Some(Secret::Replacement)
        );
        assert_eq!(
            forced.consume("\r\nRetype new password: ").unwrap(),
            Some(Secret::Replacement)
        );
        assert_eq!(
            forced.consume("\r\npasswd: password updated successfully\r\n"),
            Ok(None)
        );
        assert!(forced.rotated);
    }

    /// A success message before any confirmation is not a rotation, and the
    /// enrollment marker is recognised wherever it arrives.
    #[test]
    fn success_needs_a_confirmation_and_the_marker_reports_enrollment() {
        let mut dialogue = Dialogue::after_login();
        assert_eq!(
            refused(dialogue.consume("password updated successfully\n")),
            "unexpected_password_success"
        );
        let mut enrolled = Dialogue::after_login();
        let text = format!("banner\r\n{ENROLLED}\r\n");
        assert_eq!(enrolled.consume(&text), Ok(None));
        assert!(enrolled.enrolled && !enrolled.rotated);
    }
}
