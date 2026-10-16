# Channels

A channel is somewhere the agent can be reached. There are six, and they come in
two shapes that are worth separating before you set any of them up, because
choosing the wrong one is how people end up paying for something they already
had.

| | **Yours** | **The agent's own** |
| --- | --- | --- |
| WhatsApp | the number on your phone | a Meta Cloud number, via Kapso |
| Telegram | your account | a bot from BotFather |
| Email | your Gmail | an AgentMail address |

**Yours** means the agent acts *as you*, on an account you already have. It is
paired the way a second device is, costs nothing, and nobody new can reach you.

**The agent's own** means it has an identity other people can write to — a
number, a bot, an address that is not yours. That needs a credential, sometimes
costs money, and for WhatsApp needs your machine to be reachable from outside.

Neither substitutes for the other. Pick per channel.

---

## Your own WhatsApp — free

The agent reads and replies on the number already on your phone.

1. Run the `whatsapp` skill's `login` command.
2. Scan the QR code with the phone holding the account, exactly as you would for
   WhatsApp Web.

There is no key and nothing to add to `.env`. The session is a paired device and
expires the way WhatsApp Web does; re-run `login` when it does.

## A WhatsApp number for the agent — paid

A number of the agent's own that other people can message.

1. Create a project and a number at <https://kapso.ai>.
2. Add to `$MAGICIAN_ROOT_DIR/.env`:
   ```
   KAPSO_API_KEY=...
   ```
3. Copy the **number-webhook signing secret** from Kapso → Project Settings and
   add that too. Inbound messages fail closed until it is set — an unsigned
   webhook is one anybody could have sent, so refusing is the correct behaviour
   rather than a bug.
4. Set up the [public tunnel](../quickstart.md), because Kapso has to reach your
   machine. This is the one channel that needs it.

## Your own Telegram — free

The agent reads and replies on your account.

1. Sign in at <https://my.telegram.org/apps> and create an application.
2. Add to `.env`:
   ```
   TGCLI_API_ID=...
   TGCLI_API_HASH=...
   ```
3. Start the `telegram-self` bot, click **Authenticate** in Bot Control, and scan
   the QR code with the phone holding the account.

The API credentials are yours from Telegram and cost nothing. The pairing is the
second step and the one that actually connects it — credentials alone are not a
session.

## A Telegram bot for the agent — free

1. Message [@BotFather](https://t.me/BotFather) and send `/newbot`.
2. Pick a name and a username ending in `bot`.
3. Add the token it gives you:
   ```
   TELEGRAM_TOKEN=123456789:ABC...
   ```

A bot cannot read a group chat it has not been added to, and in groups it only
sees messages addressed to it unless you turn off privacy mode in BotFather.
That is Telegram's rule, not ours, and it is usually the answer to "why did it
not see that message".

## An email address for the agent — free tier

1. Create an inbox at <https://agentmail.to>.
2. Add to `.env`:
   ```
   AGENT_MAIL_KEY=...
   MAGICIAN_AGENT_EMAIL=your-agent@agentmail.to
   ```

The second line is not optional bookkeeping — it is how the agent knows which
address is its own, and therefore which mail is addressed to it rather than
merely visible to it.

## Your Gmail — free tier

Reading and drafting in your own mailbox is OAuth rather than a key, and it has
[its own guide](google-workspace.md) because the Google Cloud side is longer
than everything above put together.

---

## Checking

```bash
make setup-wizard ARGS=--status
```

Every channel appears as a component with its cost and whether it is configured.
Running the wizard without `--status` lets you pick the capability — "use your
own WhatsApp", "give the agent its own email" — and it works out which
credential that needs, which is the direction most people want to think in.
