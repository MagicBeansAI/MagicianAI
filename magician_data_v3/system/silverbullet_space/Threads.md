# Threads

Notes are filed under `Threads/`. SilverBullet does not index a folder on
its own, so this page is the index and stays live as notes are added.

${query[[
  from p = index.pages()
  where string.startsWith(p.name, "Threads/")
  order by p.name desc
  limit 200
  select templates.pageItem(p)
]]}
