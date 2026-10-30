# Cloudflare setup

Cloudflare is optional. Local Magician works without it. Use a domain you
control for stable HTTPS access from phones, remote Desktop Edge clients,
browsers and webhook providers. No operator domain, account or tunnel is bundled.

## One tunnel per computer

In the terminal wizard or Desktop setup, select **Reach this computer remotely**.
The shared catalog collects your account, domain, machine label, email allowlist
and masked API token, then offers the existing per-machine provisioner. Its final
check calls your runtime's `/health` through Access using the saved service
credential; an old URL file or Access login redirect cannot satisfy readiness.

The equivalent shell setup is `make setup-cloudflare`. It asks only for missing
values, writes both private runtime env files and provisions the selected
machine. Use `ARGS=--non-interactive` with preconfigured values in automation,
or `ARGS=--dry-run` to validate and preview without saving or provisioning.

To move this computer onto a different domain, run `make setup-cloudflare`
again and confirm the switch, then enter the new zone. Setup saves that zone,
clears saved hostnames from the previous zone, rebuilds this computer’s tunnel,
and runs the original shared tunnel script. Restart Magician and pair phones
again. Kapso and AgentMail register the new webhook when Magician starts. One
zone is in use at a time. DNS records and Access apps on the previous zone stay
until each hostname is parked:

```bash
make machine-tunnel PARK=laptop.old-example.com
```

The terminal wizard asks the same switch before it re-runs the tunnel step.
Desktop setup saves the new zone on the account step, then offers the switch
on the tunnel step. Declining the switch keeps the current zone. If hostnames
are still pinned to another zone, setup clears them and finishes the move.

Install `cloudflared` (`make setup-all` includes it on macOS), add your domain
to Cloudflare, and set up a Zero Trust organization. Create an API token scoped
to your account and zone with these permissions:

- Account · Cloudflare Tunnel · Edit
- Zone · Zone · Read (the setup script looks up your zone by name)
- Zone · DNS · Edit
- Account · Access: Apps and Policies · Edit
- Account · Access: Service Tokens · Edit
- Account · Access: Organizations, Identity Providers, and Groups · Read

See Cloudflare's [API token instructions](https://developers.cloudflare.com/fundamentals/api/get-started/create-token/)
and [account ID lookup](https://developers.cloudflare.com/fundamentals/account/find-account-and-zone-ids/).
The API token provisions resources; it is different from the connector token
that runs a tunnel and the Access service credential that authorizes devices.

Save these values in your private runtime `.env` and `.env.development`
(`$MAGICIAN_ROOT_DIR`, default `~/MagicianNotes`). Use the Skills page's
**Environment** tab or edit the private files; keep the files readable only by
your account. Replace every placeholder below with your own value.

```dotenv
MAGICIAN_TUNNEL_ZONE=example.com
MAGICIAN_CF_ACCOUNT_ID=your-account-id
CLOUDFLARE_API_TOKEN=your-scoped-api-token
MAGICIAN_UI_ACCESS_EMAILS=you@example.com
```

On the computer that will serve the tunnel, run:

```bash
make machine-tunnel MACHINE=laptop INSTALL=1
make restart-magician
```

Choose your own machine label. This creates or updates Cloudflare resources,
installs the connector on this computer, saves the generated credentials in
private runtime files and configures Access. Each computer needs its own label:
Cloudflare distributes requests across connectors sharing a tunnel, which can
otherwise send pairing and OAuth callbacks to the wrong computer.

| Example hostname | Purpose | Protection |
| --- | --- | --- |
| `laptop.example.com` | Runtime API and host gateway | Access service credential plus Magician device authorization |
| `ui-laptop.example.com` | Development browser UI on port 5173 | Access email allowlist plus Magician sign-in |
| `webhook-laptop.example.com` | Kapso and AgentMail callbacks | Provider authentication; no interactive Access login |

Tool-declared public MCP profiles use this computer’s HTTPS hostname at
`/.well-known/magican/mcp-profiles/<content-hash>.json`. Setup routes only these
static JSON documents without Access; the API and other paths retain their
normal authentication. Shopify profile URLs are supplied automatically.

The UI host requires the development UI to be running. The tunnel does not
replace Magician sign-in or device pairing. Pair devices through Settings after
setup. Provider callbacks need no manual step: after a Magician restart the
Kapso and AgentMail bots register their webhooks on the new webhook hostname.
Never commit runtime env files, `~/.cloudflared` credentials, tokens, account
IDs or your deployment hostnames into configuration examples or screenshots.

Verify that the connector is healthy in Cloudflare, that the UI hostname asks
for Access authentication, and that a paired device reaches the intended
computer. Provider webhook URLs must reach their handler without an Access
login redirect; unauthenticated provider requests should still be rejected.

## Other uses

For a webhook-only tunnel or manual dashboard/browser setup, see the
[quick start's tunnel modes](../quickstart.md#cloudflare-tunnel--kapso-webhook-opt-in).
VibeDev's **Deploy** settings manage Cloudflare Pages credentials separately;
publishing a Pages project does not require exposing your runtime through a tunnel.
