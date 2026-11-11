# Schedules

Schedules are optional prompt templates, not executables or automatically
installed automation. Materialize `<installed-skill>`, `<python-launcher>` and an
available user-configured dispatch route before creating or refreshing one.
Stop before claiming if any value is unresolved. See
[integrations](integrations.md) for setup and explicit refresh by the
existing schedule ID. Reinstalling the skill never refreshes stored prompts.
A host acts only on projects with local checkouts in `paths.md`. `no ideas`
means no runnable ideas here, not an empty board. Unassigned ideas stay put.
Ideas are picked up by `factory.py pickup`, a model-free host timer; see [references/pickup.md](pickup.md). `schedules/resume.md` and `schedules/explore.md` are the shipped prompt templates.
A failed dispatch returns its idea with `unclaim`.
