# Magican Notes

- [[Inbox]]
- [[Captures]]
- [[Tasks]]
- [[Threads]]
- [[Artifacts]]
- [[Programs]]
- [[Sources]]

## Recently updated

${query[[
  from p = index.pages()
  order by p.lastModified desc
  limit 25
  select templates.pageItem(p)
]]}

## Every page

The curated links above are convenience. This listing is the safety net: a
folder is not a page in SilverBullet, so a section whose index page is missing —
or a new section nobody has linked yet — would otherwise be reachable only by
typing its URL. Nothing here depends on that list being kept up to date.

${query[[
  from p = index.pages()
  order by p.name
  select templates.pageItem(p)
]]}
