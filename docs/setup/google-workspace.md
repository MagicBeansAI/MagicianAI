# Connecting a Google account

Mail assist reads and drafts in your mailbox. That needs a Google Cloud project
of your own — not because Magican wants one, but because Google will not hand a
desktop app access to your mail without one, and using someone else's project
would mean your mail flowing through their credentials.

It stays yours. The OAuth client is created in your project, the token lives on
your machine, and nothing here is shared infrastructure.

Budget fifteen minutes. Most of it is waiting for the consent screen form.

---

## 1. A project

<https://console.cloud.google.com/projectcreate>

Name it anything. Note the **Project ID** — the string Google generates, not the
display name; they differ, and the ID is the one everything else uses.

## 2. Enable two APIs

Both, in this project:

- **Gmail API** — <https://console.cloud.google.com/apis/library/gmail.googleapis.com>
- **Cloud Pub/Sub API** — <https://console.cloud.google.com/apis/library/pubsub.googleapis.com>

Pub/Sub is not optional and is easy to skip. The bot does not poll your mailbox;
it asks Gmail to *push* changes, and Gmail pushes to a Pub/Sub topic. Without
this API, authentication succeeds and mail never arrives — a failure that looks
like nothing happening rather than like an error.

## 3. The consent screen

<https://console.cloud.google.com/apis/credentials/consent>

- **User type: External.** Internal only exists if you have Workspace, and
  External works either way.
- Fill in app name, your support email, your developer email. Nothing else on
  this page matters for a personal install.
- **Publishing status: leave it in Testing.** Do not click "Publish app". A
  published app faces Google's verification review, which for these scopes takes
  weeks. Testing works immediately.
- **Test users → Add users → your own Google address.**

That last step is the one everybody misses. In Testing mode, an account that is
not a listed test user is refused at sign-in with `access_denied`, and the
message does not say why. Add every account you intend to connect, including
your own.

Testing-mode refresh tokens expire after seven days. For a personal install that
means re-authenticating weekly; publishing removes it, at the cost of review.

## 4. The OAuth client

<https://console.cloud.google.com/apis/credentials> → **Create Credentials →
OAuth client ID → Desktop app**.

Download the JSON and save it as:

```
$MAGICIAN_ROOT_DIR/client_secret.json      # default: ~/MagicianNotes/client_secret.json
```

That file is gitignored and never leaves your machine. The project ID is read
out of it, so there is nothing else to configure.

## 5. The Pub/Sub topic

<https://console.cloud.google.com/cloudpubsub/topic/list> → **Create topic**.
Any name; `magician-gmail` is a reasonable one.

Then the permission that makes it work. On the topic, **Add principal**:

- **Principal:** `gmail-api-push@system.gserviceaccount.com`
- **Role:** `Pub/Sub Publisher`

This is Google's own service account, and it is how Gmail is allowed to publish
to your topic. Without it, the watch registers, Gmail tries to publish, and the
push is silently dropped — again with no error anywhere you would think to look.

Create a **subscription** on the topic as well; the default pull subscription is
fine.

## 6. Connect

```bash
make setup-all
```

A browser opens. Sign in as a test user from step 3, and grant both scopes:

- `gmail.modify` — read, label and draft. Not `gmail.send`: nothing is sent
  without you approving it.
- `pubsub` — subscribe to the change notifications from step 5.

The token lands in an isolated `auth/gws-<name>` directory under your data root.

---

## When it does not work

**`access_denied` at sign-in.** The account is not a test user. Step 3, and the
address has to match exactly.

**Auth succeeds, no mail ever arrives.** Almost always the Pub/Sub Publisher
grant in step 5, or the Pub/Sub API not enabled in step 2. Both fail quietly by
design — Gmail does not report a failed push back to you.

**It worked, then stopped after a week.** Testing-mode refresh tokens expire
after seven days. Re-run `make setup-all`, or publish the app.

**Wrong account connected.** `GWS_EXPECTED_EMAIL` pins which identity a bot will
run as, and it refuses rather than acting as the wrong person.
