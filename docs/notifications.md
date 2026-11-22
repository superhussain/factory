# Notifications and the installed app

This page sets up Web Push, installs the dashboard as an app and lists every way the factory alerts you. The example is made up: Ada (`ada@example.test`) runs the instance `https://factory.example.test` with the worker `factory-example-test`, and her job `051-sample-retry`, "Retry the sample upload", starts waiting on her while her iPhone is locked.

## Channels at a glance

| Channel | Reaches you when | Set up by |
| --- | --- | --- |
| Browser notification | the dashboard tab is open in the background | each person, per browser, in Settings, Notifications |
| Toasts and sounds | the dashboard tab is open | each person, per browser, in Settings, Notifications, Events |
| Web Push | the dashboard is closed | the operator once per instance, then each person per browser |
| App icon badge | the installed app is open | each person, by installing the app |
| Paseo needs-you alerts | a Paseo window is open | each person, in Paseo |

## Set up the push key

Push is off until the Worker has the `FACTORY_VAPID_KEY` secret, a private EC P-256 JWK. The operator does this once per instance. Generate the key and store it in one pipe, so the private key never shows on the terminal or touches disk:

```sh
node tools/vapid-key.mjs 2>/dev/null | pnpm exec wrangler secret put FACTORY_VAPID_KEY --name <worker name>
```

- `<worker name>` is `worker_name` in `deploy/profile.json`. Ada's is `factory-example-test`.
- `tools/vapid-key.mjs` writes the private JWK to stdout, into the pipe. It writes the public key to stderr, which this command discards.
- The Worker parses the secret and needs `kty` `EC`, `crv` `P-256` and 32-byte `x`, `y` and `d`. Any other value leaves push off.
- The Worker serves the public key at `GET /api/push/key` to signed-in people only. Device tokens get 404. Without a valid key the route answers 404 and nothing is sent.
- Without the key, Settings shows "Push is not set up on this instance."
- To turn push off, delete the secret: `pnpm exec wrangler secret delete FACTORY_VAPID_KEY --name <worker name>`.
- To rotate, put a new key the same way. Then every browser must turn push off and on again to subscribe with the new key.

## Turn on push in a browser

Each person opts in once per browser. Open Settings, Notifications and check "Push to this device when the dashboard is closed". The browser asks for notification permission.

- If you blocked notifications, allow them in the browser's site settings, then check the box again.
- Each browser stores one subscription under `push/` in R2.
- An instance holds at most 20 subscriptions. The 21st shows "This instance already has 20 push devices. Turn push off on one first."
- Every subscribed browser gets every push, whoever subscribed it. Ada's teammates' browsers get her `051-sample-retry` push too.
- Unchecking the box removes this browser's subscription.
- When the push service answers 404 or 410 for a subscription, the Worker deletes that stale record.

## What notifies

| Event | Channel | Message |
| --- | --- | --- |
| A board write makes a job wait on you: stage `needs-human`, a `needs` value, or an escalated `pr-open` | Web Push | `Needs you: 051-sample-retry` |
| A job reaches `merged` | Web Push | `Merged: <id>` |
| The hourly cron (`17 * * * *`) finds an in-flight job whose claim went stale 240 minutes earlier | Web Push, once per stale claim | `Stuck: <id>` |
| The hourly cron finds a job that has waited on you 24 hours since its last board update | Web Push, once per wait | `Still needs you: 051-sample-retry` |
| A job starts waiting on you while the tab is in the background | Browser notification, with "Notify me when a job needs me" on | the job id and title |
| A job still waits after 1 hour, 4 hours or 1 day | Browser notification, with a reminder picked | one more notification |
| A new backlog item, a job needs you, a spec is ready, a pull request opens, a job merges | Toasts and sounds, set per event in Events | a toast in the tab, a sound or both |

- A push body is the job title only, so Ada sees "Retry the sample upload" under `Needs you: 051-sample-retry`.
- Several events in one board write become one push titled `<n> factory updates`.
- Tapping a push opens that job in the dashboard.

## Install the dashboard as an app

Installing is per browser. Nothing is set on the server.

- iPhone or iPad: open the dashboard in Safari, tap Share, then Add to Home Screen. Safari never prompts.
- Android Chrome: open the menu and tap Add to Home screen or Install app. Chrome shows its own banner only after its engagement checks pass.
- Desktop Chrome or Edge: use the install icon in the address bar.
- Settings, Display, Install app shows Chrome's install prompt once the browser has offered one. Otherwise it shows a hint.
- The manifest, icons and `/sw.js` sit behind the same Access check as every other route. Android Chrome may fetch the icons without the Access cookie and add a plain shortcut instead of an app icon.

## iPhone and iPad

- Push reaches iPhone and iPad only in the dashboard added to the Home Screen and opened from there.
- In a Safari tab, the push setting shows "This browser cannot receive push. On iPhone or iPad, add the dashboard to your Home Screen and open it from there."
- The badge shows only after the Home Screen app is allowed to send notifications.

## App icon badge

- The installed app's icon shows the needs-you count, the same number as the tab title.
- The badge clears at zero and when sign-in expires.
- It updates only while the app is open.
- Browsers without the Badging API show no badge.

## Paseo needs-you alerts

People who work in Paseo can get alerts there instead, with no VAPID key. Paseo shows a notification such as "Needs you: 051-sample-retry" while a Paseo window is open. See [integrations.md](integrations.md#needs-you-alerts). Web Push still covers phones and closed windows.
