// Reports whether the Worker serving a hostname has a Workers AI binding. Prints only
// `ai binding: yes|no` and the binding name:type pairs. Usage:
// node tests/live/deployed-ai-binding.mjs <hostname> with CLOUDFLARE_API_TOKEN and
// CLOUDFLARE_ACCOUNT_ID in the environment.
const hostname = process.argv[2];
const { CLOUDFLARE_API_TOKEN: token, CLOUDFLARE_ACCOUNT_ID: account } = process.env;
if (!hostname || !token || !account) {
  console.error(
    "usage: node tests/live/deployed-ai-binding.mjs <hostname> with CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID set",
  );
  process.exit(2);
}

async function api(path) {
  const response = await fetch(`https://api.cloudflare.com/client/v4/accounts/${account}${path}`, {
    headers: { Authorization: `Bearer ${token}` },
  });
  const body = await response.json();
  if (!body.success)
    throw new Error(`${response.status}: error codes ${body.errors.map((e) => e.code).join(", ")}`);
  return body.result;
}

const [domain] = await api(`/workers/domains?hostname=${encodeURIComponent(hostname)}`);
if (!domain) throw new Error(`no Worker domain for ${hostname}`);
const { bindings } = await api(`/workers/scripts/${encodeURIComponent(domain.service)}/settings`);
console.log(`ai binding: ${bindings.some((b) => b.type === "ai") ? "yes" : "no"}`);
for (const b of bindings) console.log(`${b.name}:${b.type}`);
