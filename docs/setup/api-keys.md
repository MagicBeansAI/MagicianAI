# Keys and paid services

Magican runs without any of these. Chat needs one model provider; everything
below that is a capability you switch on, and nothing here fails in a way that
breaks the rest.

**What costs money.** Every table below marks it, using three words and no
numbers:

| | |
| --- | --- |
| **free** | no account, no card. The local model is the whole of this column. |
| **free tier** | free to start, metered after. Enough to try without deciding. |
| **paid** | billed from the first call. |

Real prices change weekly, vary by usage, and are the provider's to state — a
number written here would be wrong within a month, so each row links the place
that has the current one. Nothing charges you without a key you put there
yourself.

The setup wizard marks the same three words beside each component, so what you
read here is what it says at the moment of choice.

Keys live in one file:

```
$MAGICIAN_ROOT_DIR/.env        # default: ~/MagicianNotes/.env
```

It is gitignored, never leaves your machine, and `magician_data_v3/.env.example`
is the annotated list of everything that can go in it. Keep it at mode `0600`.

A key set in your shell environment wins over the file, which is useful for a
one-off and a good way to confuse yourself permanently. Prefer the file.

---

## Model providers — pick one

This is the only section that is not optional. Chat needs a model, and one key
is enough.

| Provider | Variable | Cost | Where |
| --- | --- | --- | --- |
| OpenAI | `OPENAI_API_KEY` | paid | <https://platform.openai.com/api-keys> · [pricing](https://openai.com/api/pricing/) |
| Anthropic | `ANTHROPIC_API_KEY` | paid | <https://console.anthropic.com/settings/keys> · [pricing](https://www.anthropic.com/pricing) |
| Google Gemini | `GEMINI_API_KEY` | free tier | <https://aistudio.google.com/apikey> · [pricing](https://ai.google.dev/pricing) |
| DeepSeek | `DEEPSEEK_API_KEY` | paid | <https://platform.deepseek.com/api_keys> |
| MiniMax | `MINIMAX_API_KEY` | paid | <https://www.minimax.io/platform> |
| Sarvam AI | `SARVAM_API_KEY` | paid (₹100 free credit) | <https://dashboard.sarvam.ai> · [pricing](https://docs.sarvam.ai/api-reference-docs/pricing) — Indic-language chat (`chat-sarvam-adaptive`), text-only |
| OpenRouter | `OPENROUTER_API_KEY` | free tier | <https://openrouter.ai/keys> — one key, many models, some free |

```bash
echo 'OPENAI_API_KEY=sk-...' >> "$MAGICIAN_ROOT_DIR/.env"
```

The setup wizard reports a model provider as present when **any** of these is
set, so you are not choosing a blessed one.

**The local alternative.** A local model needs no key at all, and it is the only
way to keep classification and distillation on your machine. It also needs 16 GB
on Apple Silicon and several gigabytes of disk — see
[the component graph](../../component-graph.html) for what that trades.

---

## Web research

Optional, and the agent degrades to what it can reach without them.

| Service | Variable | Cost | Notes |
| --- | --- | --- | --- |
| TinyFish | `TINYFISH_API_KEY` | free tier | preferred |
| Exa | `EXA_API_KEY` | paid | fallback |
| Tavily | `TAVILY_API_KEY` | paid | fallback |

One is enough. The wizard offers this as **Web search API** — a browser can open
a page you name, and a search index is how the agent decides which page.

## Media generation

| Service | Variable | Cost | Makes |
| --- | --- | --- | --- |
| Nano Banana 2 | `NANOBANANA2_API_KEY` | paid | images |
| Veo 3.1 | `VEO31_API_KEY` | paid | video |
| MiniMax | `MINIMAX_API_KEY` | paid | images, video, music, speech |
| Klipy | `KLIPY_API_KEY` | free tier | GIFs and memes |

Generation is the expensive kind of call, and nothing generates without you
asking for it.

Nano Banana and Veo are Google models, so both take a Gemini API key — the same
one from the table above works for all three.

## Higgsfield

The odd one out: it authenticates through its own CLI rather than an
environment variable.

```bash
# 1. install and log into the Higgsfield CLI (its own instructions)
# 2. bind it to a scope
make setup-higgsfield-cli SCOPE=<principal>/<workspace>
```

That seeds the scoped login and binds the reviewed executable. There is no
`HIGGSFIELD_API_KEY`; if you go looking for one you will not find it.

## Channels

| Channel | Variable | Cost | Notes |
| --- | --- | --- | --- |
| AgentMail | `AGENT_MAIL_KEY` | free tier | the agent's own inbox, separate from yours |
| Kapso | `KAPSO_API_KEY` | paid | a WhatsApp number **for the agent** |
| Telegram | `TELEGRAM_TOKEN` | free | from @BotFather |

Every channel is set up in [its own guide](channels.md), which has the steps.
The keys below are what those steps ask you to paste.

### The two WhatsApps are not the same thing

Worth separating, because the word covers two capabilities with different costs
and neither substitutes for the other:

| | Your own WhatsApp | The agent's WhatsApp |
| --- | --- | --- |
| Whose number | the one on your phone | a new one, the agent's |
| How | scan a QR code, like WhatsApp Web | Kapso / Meta Cloud |
| Costs | **free** — no key at all | **paid**, plus the public tunnel |
| Who can message it | you, as you | anyone you give the number to |
| Set up with | the `whatsapp` skill's `login` | `KAPSO_API_KEY` + webhook secret |

The first is the agent acting **as you** on a number you already have. The
second gives it a number of **its own** that other people can message. The
wizard offers them as two separate capabilities for that reason.

Kapso needs a second value: the number-webhook signing secret from **Kapso →
Project Settings**. Inbound webhooks fail closed while it is unset, which is
deliberate — an unsigned inbound message is one anybody could have sent.

WhatsApp also needs the tunnel, because Kapso has to reach your machine.

Gmail is not a key at all. It is OAuth, and it has
[its own guide](google-workspace.md).

## Other

| Service | Variable | Cost | For |
| --- | --- | --- | --- |
| GitHub | `GITHUB_TOKEN` | free | repository skills |
| Metabase | `METABASE_API_KEY` | free | your own Metabase |
| CloakBrowser | `CLOAKBROWSER_LICENSE_KEY` | paid | one concurrent stealth session |
| Cloudflare | `CLOUDFLARE_API_TOKEN` | free tier | tunnel and Access automation |

---

## Checking what took

```bash
make setup-wizard ARGS=--status
```

It reports each component as present or absent with the reason, and marks the
paid ones, so a key that did not land shows up as a missing capability rather
than as a failure later.

Running the wizard without `--status` lets you pick capabilities rather than
keys — tick "search the web" or "make images and video" and it tells you which
key that needs and what it costs, instead of asking you to work backwards from
a list of providers.

A key is read at startup. Add one to a running stack and restart before
expecting it to matter.
