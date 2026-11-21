# Assistant

The Assistant is the add-on that reads incoming messages and proposes what to do with them
([ADR 0006](../../docs/adr/0006-assistant.md)). It never acts by itself: every proposal waits for
an agent, who accepts, edits or discards it. An accepted draft reply goes into the agent's composer;
the agent sends it.

## What it proposes

| Event | What it reads | What it may propose |
|---|---|---|
| `message.received`, message in the inbox | the sender, the customers with their mail domains, the sender's open tickets | **triage**: open a ticket for a customer, attach the message to one of the sender's tickets, or discard it (spam, newsletter, automatic reply) |
| `message.received` on a ticket, and `ticket.opened` | the ticket's messages (not its internal notes), the customer's active assets | **priority**, **assets** involved, a **draft reply** in the language of the requester's latest message |

The model's answer is checked before anything is proposed, and whatever does not pass is dropped:

- a customer, asset or ticket the model was not shown;
- a ticket opened for a customer without words in the message that name it: the model must quote
  them, the quote must appear in the message and contain the customer's name or one of its mail
  domains (a domain that merely resembles a customer's is not enough);
- an answer that is not the JSON asked for, that was cut off, or that the model declined to give;
- anything that would change nothing: the priority the ticket already has, assets already linked;
- a draft reply when the ticket has no requester, or when the help desk wrote last.

When the model is unsure it is asked to propose nothing, which is the answer agents prefer to a
wrong one.

The prompts are in [`src/prompts.rs`](src/prompts.rs). The model sees messages as data and is told
never to follow instructions inside them; internal notes are never shown to it, so they cannot end
up in a draft reply.

## Registering it on the core

```sh
hd addon add assistant --url http://127.0.0.1:8090 \
  --event message.received --event ticket.opened \
  --scope directory:read --scope tickets:read --scope proposals:write
```

The core prints a token and an event secret once: put them in `HD_ASSISTANT_TOKEN` and
`HD_ASSISTANT_EVENT_SECRET`. `hd addon disable assistant` switches it off (its events wait),
`hd addon enable assistant` switches it back on.

## Configuration

Environment variables, also read from a `.env` file in the working directory or a parent:

| Variable | Default | Meaning |
|---|---|---|
| `HD_ASSISTANT_LISTEN` | `127.0.0.1:8090` | Where it receives events (`POST /events`). |
| `HD_ASSISTANT_CORE_URL` | required | The core's base URL, such as `https://helpdesk.example.com`. |
| `HD_ASSISTANT_TOKEN` | required | The token printed by `hd addon add`. |
| `HD_ASSISTANT_EVENT_SECRET` | required | The event secret printed by `hd addon add`. |
| `HD_ASSISTANT_MODEL_PROVIDER` | `anthropic` | `anthropic` (the Claude API) or `openai-compatible` (any server with `/v1/chat/completions`, such as Ollama). |
| `HD_ASSISTANT_MODEL` | `claude-opus-5-5` for `anthropic` | The model's id; required for `openai-compatible`. |
| `HD_ASSISTANT_MODEL_URL` | `https://api.anthropic.com` for `anthropic` | The model server's base URL, with or without `/v1`; required for `openai-compatible`. |
| `HD_ASSISTANT_API_KEY` | required for `anthropic` | The model service's key; optional for `openai-compatible`. |
| `HD_ASSISTANT_MODEL_EFFORT` | `medium` | Claude API only: how much the model thinks before answering (`low` to `max`); set it empty to send none, for models that do not take it. |
| `HD_ASSISTANT_LOG` | `info` | A `tracing` filter. |

With the Claude API, answers are constrained to a JSON schema (structured outputs), and on the
models that support it a request declined by the API's safety classifiers is retried server-side
on a fallback model (`fallbacks: "default"`). With an OpenAI-compatible server the schema is given
in the instructions and the server is asked for a JSON object; the answer is validated in either
case.

For a model on the help desk's own machine, for example with Ollama:

```sh
HD_ASSISTANT_MODEL_PROVIDER=openai-compatible
HD_ASSISTANT_MODEL_URL=http://127.0.0.1:11434
HD_ASSISTANT_MODEL=<a model you have pulled>
```

## Privacy

To make its proposals the Assistant sends the model the messages it reads: customers' mail, with
senders' names and addresses, the customer's name and its assets. With the default provider that
means sending them to the Claude API, an external service. Whether a help desk may do that is its
own data-protection decision; the Assistant logs a warning at startup whenever its model is not on
this machine or a private network. To keep the mail in house, run a model on your own servers and
use the `openai-compatible` provider.

## Delivery and duplicates

The core waits at most 10 seconds for an answer to an event, while the model can take longer. So
the Assistant checks each event's signature and timestamp, answers at once, and works on it in the
background, at most four at a time. The core may deliver an event more than once; the Assistant
remembers the ids of the last 10,000 events it took and skips repeats. Since the core considers an
answered event delivered, the Assistant itself retries an event whose work meets an unavailable
core or model (after 30 seconds, 2 minutes and 10 minutes); events are not kept across restarts.

## Development

```sh
cargo run -p hd-assistant          # with the variables above
cargo test -p hd-assistant         # against a fake core and a fake model; no key needed
```
