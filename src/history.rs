use crate::api::Content;

const MAX_HISTORY_LEN: usize = 10;

/// Trims history to the last `MAX_HISTORY_LEN` messages, ensuring the first
/// retained message has `role: "user"` (Gemini API requirement).
pub fn prune(history: &mut Vec<Content>) {
    if history.len() <= MAX_HISTORY_LEN {
        return;
    }
    let mut start = history.len() - MAX_HISTORY_LEN;
    if start < history.len() && history[start].role != "user" {
        start += 1;
    }
    *history = history[start..].to_vec();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Part;

    fn msg(role: &str, text: &str) -> Content {
        Content {
            role: role.to_string(),
            parts: vec![Part {
                text: text.to_string(),
            }],
        }
    }

    #[test]
    fn no_pruning_under_limit() {
        let mut h = vec![msg("user", "a"), msg("model", "b")];
        prune(&mut h);
        assert_eq!(h.len(), 2);
    }

    #[test]
    fn prunes_to_max_len() {
        let mut h: Vec<Content> = (0..15)
            .map(|i| {
                if i % 2 == 0 {
                    msg("user", &format!("u{i}"))
                } else {
                    msg("model", &format!("m{i}"))
                }
            })
            .collect();
        prune(&mut h);
        assert!(h.len() <= MAX_HISTORY_LEN + 1);
    }

    #[test]
    fn first_message_after_prune_is_user() {
        let mut h: Vec<Content> = (0..20)
            .map(|i| {
                if i % 2 == 0 {
                    msg("user", &format!("u{i}"))
                } else {
                    msg("model", &format!("m{i}"))
                }
            })
            .collect();
        prune(&mut h);
        assert_eq!(h[0].role, "user");
    }

    #[test]
    fn prune_at_exact_boundary() {
        let mut h: Vec<Content> = (0..MAX_HISTORY_LEN)
            .map(|i| {
                if i % 2 == 0 {
                    msg("user", &format!("u{i}"))
                } else {
                    msg("model", &format!("m{i}"))
                }
            })
            .collect();
        let original_len = h.len();
        prune(&mut h);
        assert_eq!(h.len(), original_len);
    }

    #[test]
    fn clear_resets_to_empty() {
        let mut h = vec![msg("user", "a"), msg("model", "b")];
        h.clear();
        assert!(h.is_empty());
    }
}
