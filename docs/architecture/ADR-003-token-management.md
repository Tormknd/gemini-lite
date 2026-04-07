# ADR-003: Conversation Token Management — Sliding Window

## Status

Accepted

## Context

The Gemini API charges per token (input + output). In a multi-turn conversation, the full history is sent with every request. Without pruning, a 50-message conversation would send ~50× the original prompt tokens, leading to:

1. **Cost explosion** on paid tiers.
2. **Hitting the context window limit** (1M tokens for Flash, but effective quality degrades well before that).
3. **Increased latency** as the model processes more input.

### Alternatives Considered

| Approach | Pros | Cons |
|---|---|---|
| **No pruning** | Full context preserved | Unbounded cost growth. Will eventually hit API limits. |
| **Automatic summarization** | Preserves semantic context in compressed form | Requires an extra API call per message (more cost, more latency). Summarization quality is unpredictable. Adds significant code complexity. |
| **Token-based pruning** | Precise control over input size | Requires a local tokenizer (tiktoken or similar), adding a heavy dependency. Gemini's tokenizer is not publicly available. |
| **Message-count sliding window** | Simple, predictable, zero overhead | Loses older context abruptly. The cutoff is arbitrary. |
| **Manual clear (user-triggered)** | User controls when to reset | Relies on user discipline. Most users forget. |

## Decision

Use a **sliding window of the last 10 messages** combined with a **manual clear shortcut** (`Ctrl+K`).

Implementation details:
- After adding a new user message to the history, if `len > 10`, truncate from the front.
- The Gemini API requires the first message in the `contents` array to have `role: "user"`. If the truncation point falls on a `model` message, advance by one to ensure a `user` message leads.
- A `system_instruction` field (outside the `contents` array) provides persistent context that is not affected by pruning.
- A token counter in the UI shows the `totalTokenCount` from the API response, giving the user visibility into usage.

### Why 10 messages?

- 10 messages ≈ 5 user-model exchanges. Empirically, this covers the "working memory" of most task-oriented conversations.
- At ~100 tokens per message average, 10 messages ≈ 1,000 input tokens — well within the free tier's comfort zone.
- The number is a constant (`MAX_HISTORY_LEN`) in `src/history.rs`, trivially adjustable.

## Consequences

- **Positive:** API costs stay near-zero for typical usage. The free tier (15 RPM, 1M TPM for Flash) is practically never exhausted.
- **Positive:** Zero runtime overhead — no extra API calls, no local tokenizer.
- **Positive:** The user has an escape hatch (`Ctrl+K`) for when they want to start fresh.
- **Negative:** The model loses context from messages older than the window. For long debugging sessions, the user may need to re-state earlier context.
- **Trade-off:** A token-based pruning strategy would be more precise, but the added complexity (and dependency on a tokenizer) is not justified for a lightweight client.
