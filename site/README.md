# Site

The documentation site: every Markdown file in the repository, rendered with
[VitePress](https://vitepress.dev), with Mermaid diagrams drawn and local
search. Readers find it in the Workhorse hub, at
[/atelier/](https://workhorse-docs.stevenjpx2.workers.dev/atelier/), which
pulls these pages from main when it builds. Here it runs locally and checks
every pull request. The Worker atelier-docs, at
[atelier-docs.stevenjpx2.workers.dev](https://atelier-docs.stevenjpx2.workers.dev),
sends readers to the corresponding page in the hub. The /plans/ paths map to
/atelier/docs/plans/; each rule covers its own paths, so none depends on the
order Cloudflare tries them in, and any other path shows 404.html, which links
to the hub.

- `.vitepress/config.mts` finds the pages and builds the sidebar and source links.
- `.vitepress/theme/index.ts` draws Mermaid blocks in the site's colours.
- `.vitepress/theme/mermaid.css` sizes diagrams and the full-screen overlay.
- `cloudflare.config.ts` names the Worker; `wrangler.config.ts` names its folder.
- `redirect/_redirects` maps home, plan, docs and site paths to the hub.

- **Run:** `bun install`, then `bun run dev`.
- **Build:** `bun run build` writes `.vitepress/dist/`, and fails on a dead
  link.
- **Deploy:** `.github/workflows/docs.yml` builds every pull request and,
  on each push to main, deploys the redirect with CLOUDFLARE_API_TOKEN.
- A new Markdown file anywhere in the repository appears in the sidebar on
  the next build; nothing here lists pages by hand.
- **Diagrams** use Mermaid's base theme with the site's font and colours,
  light or dark. Each fits its column, but never below 75% of its natural
  size, so its text stays readable; a wider one scrolls sideways. Clicking a
  diagram opens it full screen; a click or Escape closes it.
