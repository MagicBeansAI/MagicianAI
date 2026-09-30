# Tasks

Task notes are filed under `Tasks/<date>/`. SilverBullet does not index a
folder on its own — this page is the index, and it stays live as notes are
added.

## Today

${query[[
  from p = index.pages()
  where string.startsWith(p.name, "Tasks/" .. os.date("%Y-%m-%d"))
  order by p.name desc
  select templates.pageItem(p)
]]}

## All tasks

${query[[
  from p = index.pages()
  where string.startsWith(p.name, "Tasks/")
  order by p.name desc
  limit 200
  select templates.pageItem(p)
]]}
