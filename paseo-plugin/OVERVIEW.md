Factory shows your factory dashboard on its own Paseo screen. A Factory row in the sidebar opens the board. When you open a job's session from the board, the agent opens in the same Paseo window instead of a new app window.

## Setup

- In Settings, Plugins, Factory, enter your factory dashboard URL, such as `https://factory.example.com`. Each host keeps its own URL.
- Your factory server must list this Paseo origin in its `embed_origins` profile key. Without it, the dashboard refuses to load inside Paseo.
- Your factory runs behind Cloudflare Access. Set the Access application's cookie SameSite attribute to None, or the framed board shows the Access login instead of your jobs.

## What it reads and sends

The plugin reads nothing on the daemon. It stores only the dashboard URL. The board loads in the app with your own Access session, and the plugin only receives the server and agent ids of a session link you click.

## Limits

- The board shows inside Paseo on web and desktop only. On iOS and Android the screen offers a link that opens the dashboard in the browser.
- Browsers that block third-party cookies cannot show the board in the frame. Use Sign in on the screen to open it in a tab.
