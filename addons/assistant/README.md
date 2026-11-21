# Assistant

The Assistant is the add-on that reads incoming messages and proposes what to do with them
([ADR 0006](../../docs/adr/0006-assistant.md)). It never acts by itself: every proposal waits for
an agent, who accepts, edits or discards it. An accepted draft reply goes into the agent's composer;
the agent sends it.

## What it proposes

| Event | What it reads | What it may propose |
|---|---|---|
| `message.received`, message in the inbox | the sender, the customers with their mail domains, the sender's open tickets | **triage**: open a ticket for a customer, attach the message to one of the sender's tickets, or discard it (spam, newsletter, automatic reply) |
| `message.received` on a ticket, and `ticket.opened` | the ticket's messages (not its internal notes), the customer's active assets and the services it uses | **priority**, **assets** and **services** involved, a **draft reply** in the language of the requester's latest message |

The model's answer is checked before anything is proposed, and whatever does not pass is dropped:

- a customer, asset, service or ticket the model was not shown (the model picks assets and
  services by short references, A1 or S1; one unknown reference drops that whole proposal);
- a ticket opened for a customer without words in the message that name it: the model must quote
  them, the quote must appear in the message and contain the customer's name or one of its mail
  domains (a domain that merely resembles a customer's is not enough);
- an answer that is not the JSON asked for, that was cut off, or that the model declined to give;
- anything that would change nothing: the priority the ticket already has, assets or services already
  linked;
- a draft reply when the ticket has no requester, or when the help desk wrote last.

When the model is unsure it is asked to propose nothing, which is the answer agents prefer to a
wrong one.

Every proposal carries a rationale, a short sentence telling the agent why, in each language of
the agents' UI, so that each agent reads it in their own. The Assistant reads those languages from
the core (`GET /api/addon/whoami`) at startup and again every ten minutes, asks the model for one
sentence per language in the same answer (written in each language, not translated word by word),
and sends them all. A language the model left out or empty is dropped, and agents working in it
read the English sentence; a proposal without an English sentence is not made. The draft reply is
not a rationale: it stays in the requester's language.

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
| `HD_ASSISTANT_MODEL_PROVIDER` | required | The service that runs the model; see the table below. |
| `HD_ASSISTANT_MODEL` | required | The model's id as the provider names it; for `azure-openai`, the deployment's name. |
| `HD_ASSISTANT_MODEL_URL` | the provider's | The service's base URL; required for `azure-openai` and `openai-compatible`. |
| `HD_ASSISTANT_API_KEY` | | The service's key; required by the hosted providers. |
| `HD_ASSISTANT_MODEL_EFFORT` | `medium` | `anthropic` only: how much the model thinks before answering (`low` to `max`); set it empty to send none, for models that do not take it. |
| `HD_ASSISTANT_AZURE_API_VERSION` | the v1 API | `azure-openai` only: a dated `api-version` (`2024-08-01-preview` or later, for structured outputs) to use the deployments path instead of Azure's v1 API. |
| `HD_ASSISTANT_LOG` | `info` | A `tracing` filter. |

There is no default provider or model: whoever installs the Assistant chooses both, since it
decides where customers' mail goes and what each message costs. The Assistant refuses to start
until both are set, and lists the providers it knows.

### Providers

| `HD_ASSISTANT_MODEL_PROVIDER` | Service | `HD_ASSISTANT_MODEL_URL` (default) | Key | Answers held to the schema |
|---|---|---|---|---|
| `anthropic` | Claude API | `https://api.anthropic.com` | required, sent as `x-api-key` | yes (structured outputs) |
| `openai` | OpenAI API | `https://api.openai.com/v1` | required, `Authorization: Bearer` | yes (`json_schema`, strict) |
| `mistral` | Mistral AI | `https://api.mistral.ai/v1` | required, `Authorization: Bearer` | yes (`json_schema`, strict) |
| `gemini` | Gemini API, OpenAI-compatible endpoint | `https://generativelanguage.googleapis.com/v1beta/openai` | required, `Authorization: Bearer` | no: JSON mode (`json_object`) |
| `azure-openai` | Azure OpenAI | required: `https://<resource>.openai.azure.com` | required, sent as `api-key` | yes (`json_schema`, strict) |
| `ollama` | Ollama | `http://127.0.0.1:11434` | none | no: JSON mode (`json_object`) |
| `openai-compatible` | any server with OpenAI chat completions (vLLM, llama.cpp…) | required: its base URL, such as `http://127.0.0.1:8000/v1` (an address alone means `/v1`) | optional, `Authorization: Bearer` | no: JSON mode (`json_object`) |

Requests go to `<url>/v1/messages` for `anthropic`, to `<url>/v1/chat/completions` for `ollama`, to
`<url>/openai/v1/chat/completions` (Azure's v1 API, with the deployment as the model) or, with a
dated version, `<url>/openai/deployments/<deployment>/chat/completions?api-version=<version>` for
`azure-openai`, and to `<url>/chat/completions` for the others.

Where a provider documents JSON Schema output on chat completions (OpenAI, Mistral, Azure) the
answer is held to the schema; it then works only on models that support structured output.
Gemini's compatibility layer shows structured output only through SDK helpers and ignores
parameters it does not know, and Ollama does not document the JSON Schema form on its
OpenAI-compatible endpoint, so those two get JSON mode. For every chat-completions provider the
schema is also written into the instructions, and whatever the provider the answer is validated
before anything is proposed: a model that does not follow the schema produces no proposal rather
than a wrong one.

With `anthropic`, on the models that support it, a request declined by the API's safety
classifiers is retried server-side on a fallback model (`fallbacks: "default"`).

For a model on the help desk's own machine with Ollama:

```sh
HD_ASSISTANT_MODEL_PROVIDER=ollama
HD_ASSISTANT_MODEL=<a model you have pulled>
```

## Privacy

To make its proposals the Assistant sends the model the messages it reads: customers' mail, with
senders' names and addresses, the customer's name, its assets and services (never internal notes). With any
hosted provider that means sending them to an external service. Whether a help desk may do that
is its own data-protection decision; the Assistant logs a warning at startup unless the model
service is on this machine: `ollama` or `openai-compatible` with a loopback address (Ollama's
default). A hosted provider always gets the warning, even behind a local proxy. To keep the mail
in house, run a model on your own servers.

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
