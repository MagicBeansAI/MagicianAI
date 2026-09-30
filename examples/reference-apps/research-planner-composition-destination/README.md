# Research Planner composition destination

This tiny external package is the reviewed destination used by the Research
Planner composition proof. Install the same immutable package into distinct
local installations when exercising a three-hop chain. Its only public action,
`accept_plan`, accepts `source_plan_id` and returns the next bounded
`forward_plan_id`; it exposes no generic request or cross-install authority.

Generate, check, test, and pack it only through the public authoring flow:

```sh
magician app check ./app --write-generated
magician app test ./app
magician app pack ./app --output ./dist/research-planner-composition-destination-0.1.0.app.zip
```

The checked-in generated paths are current owner output from
`magician app check --write-generated`. Publication, review, and approval are
separate live owner actions.
