Factory shows your factory dashboard on its own Paseo screen. A Factory row in the sidebar opens the board. When you open a job's session from the board, the agent opens in the same Paseo window instead of a new app window.

## Setup

- In Settings, Plugins, Factory, enter your factory dashboard URL, such as `https://factory.example.com`. Each host keeps its own URL.
- Your factory server must list this Paseo origin in its `embed_origins` profile key. Without it, the dashboard refuses to load inside Paseo. The desktop app's origin is `paseo://app`; the web app's is the scheme and host from your browser's address bar, such as `https://app.paseo.sh`, with no path.
- Your factory runs behind Cloudflare Access. Set the Access application's cookie SameSite attribute to None, or the framed board shows the Access login instead of your jobs.

## What it reads and sends

Once a minute the plugin runs `factory.py inbox` from this host's factory skill and sends the app the inbox count plus the id, project and title of each job waiting on you. When a new job starts waiting, Paseo web and desktop show a "Needs you" notification, if you turned notifications on under Notifications in these settings; clicking it opens that job on the Factory screen. The runtime reads the board and backlog with this host's device credentials, which never leave the host. The plugin stores only the dashboard URL. The board loads in the app with your own Access session, and the plugin only receives the server and agent ids of a session link you click.

## Limits

- Notifications need an open Paseo window on web or desktop. iOS and Android get none.
- The board shows inside Paseo on web and desktop only. On iOS and Android the screen offers a link that opens the dashboard in the browser.
- Browsers that block third-party cookies cannot show the board in the frame. Use Sign in on the screen to open it in a popup.
- Sign in opens a Paseo popup, not your system browser. The desktop app keeps its own cookies, so you sign in once there. The first time, the screen walks you through the steps; later it only asks you to sign in again. The plugin remembers that the board loaded in this browser's local storage.
