# Optional Microsoft Clarity

Clarity is optional session recording. It is off without an operator-supplied
tracking project ID. Enabling it sends UI interactions and session recordings
to Microsoft, including private app screens; review your project's masking
settings before opting in. Localhost, loopback and `.localhost` app sessions
always skip recording.

## App setup

Select **Opt in to Microsoft Clarity session recording** in the terminal wizard
or Desktop's capability setup. Create your own project at
[Microsoft Clarity](https://clarity.microsoft.com/), then enter its tracking
project ID from **Settings → Overview**. The installer stores it as
`MAGICIAN_CLARITY_PROJECT_ID` in both private runtime env files.

For the shell path:

```bash
make setup-clarity
make setup-clarity ARGS=--disable
```

Non-interactive setup accepts `MAGICIAN_ENABLE_CLARITY=1` and your own
`MAGICIAN_CLARITY_PROJECT_ID`. The full source installer offers both optional
Cloudflare and Clarity setup after core services are ready; the wizard and
Desktop present the same choices through their capability plan.

Reload the remote app page after changing this setting. No application rebuild
is required. Disable with the command above or save an empty project ID in
Skills → Environment; reload existing pages to stop already-loaded recording.
An unset or invalid ID loads no recording script. Authentication/network failure
also leaves recording off and cannot block application startup.

The authenticated `/api/magician/v2/runtime/client-config` projection returns
only the browser-visible tracking ID. Environment status remains write-only;
Cloudflare secrets and Clarity Data Export API tokens are never returned.

## Public website builds

A standalone website has a separate choice, **Configure website session
recording**, in terminal and Desktop setup. The shell equivalent is
`make setup-clarity-website`. It saves only `PUBLIC_CLARITY_PROJECT_ID` in
private runtime env files; it does not enable app recording or publish anything.

`make build-marketing-site` reads that one public setting for its next website
artifact. `make build-ui` explicitly excludes it, so opting a website in cannot
silently opt the personal app in. A directly supplied public build environment
can also configure the website. Never commit the ID or put a Data Export API
token into public build variables. The tag remains deferred until load/idle.

Disable website configuration with `make setup-clarity-website ARGS=--disable`,
then rebuild and redeploy that website; an already published static site keeps
its old setting until replaced.

Website tags are restricted to marketing routes. When recording is active or
loading, navigation across website, signed-out and app boundaries reloads the
document to discard the old recorder before the new surface appears. Ordinary
app-to-app navigation is unchanged.
