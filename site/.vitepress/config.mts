import { existsSync, readFileSync, readdirSync, statSync } from "node:fs"
import { dirname, join, relative, resolve } from "node:path"
import { fileURLToPath } from "node:url"
import { defineConfig, type DefaultTheme } from "vitepress"

const REPOSITORY = "https://github.com/wrkhse/atelier"
const BRANCH = "main"
const SITE = resolve(dirname(fileURLToPath(import.meta.url)), "..")
const ROOT = resolve(SITE, "..")
const SKIPPED = new Set([".agents", ".git", ".github", ".vitepress", "node_modules"])

/** Every Markdown file in the repository, relative to its root. */
function pages(folder = ROOT): string[] {
  return readdirSync(folder, { withFileTypes: true })
    .filter((entry) => !SKIPPED.has(entry.name))
    .flatMap((entry) => {
      const path = join(folder, entry.name)
      if (entry.isDirectory()) return pages(path)
      return entry.name.endsWith(".md") ? [relative(ROOT, path)] : []
    })
    .toSorted()
}

/** A folder's `README.md` is its index page, so folder links work. */
const rewrite = (page: string) => page.replace(/(^|\/)README\.md$/, "$1index.md")

function title(page: string): string {
  const heading = readFileSync(join(ROOT, page), "utf8").match(/^# (.+)$/m)?.[1].replaceAll("`", "")
  return heading ?? page
}

function link(page: string): string {
  return `/${rewrite(page).replace(/(^|\/)index\.md$/, "$1").replace(/\.md$/, "")}`
}

function sidebar(all: string[]): DefaultTheme.SidebarItem[] {
  const under = (prefix: string) => all.filter((page) => page.startsWith(prefix))
  const item = (page: string, text = title(page)) => ({ text, link: link(page) })
  /** A folder's pages under its title; a README is the title's own page, not an item. */
  const section = (text: string, folder: string) => {
    const index = `${folder}README.md`
    // In the order the README links them; pages it does not link come last.
    const readme = all.includes(index) ? readFileSync(join(ROOT, index), "utf8") : ""
    const at = (page: string) => {
      const found = readme.indexOf(`](${page.slice(folder.length)}`)
      return found === -1 ? Number.MAX_SAFE_INTEGER : found
    }
    const pages = under(folder)
      .filter((page) => page !== index)
      .toSorted((a, b) => at(a) - at(b) || a.localeCompare(b))
    const items = pages.map((page) => item(page))
    return all.includes(index) ? { text, link: link(index), items } : { text, items }
  }
  return [
    { text: "atelier", link: "/" },
    section("Slices", "docs/plans/"),
    { text: "Site", items: under("site/").map((page) => item(page)) },
  ]
}

/**
 * A relative link to a file the site does not render, such as a source file,
 * goes to that path on GitHub instead.
 */
function sourceLink(href: string, from: string): string {
  if (/^([a-z]+:|#|\/)/i.test(href)) return href
  // A folder's README is its index page.
  if (/(^|\/)README\.md(#|$)/.test(href)) return href.replace(/README\.md/, "")
  const [path, hash] = href.split("#")
  const target = resolve(dirname(from), decodeURI(path))
  const rendered = target.endsWith(".md") || existsSync(join(target, "README.md"))
  if (rendered || !target.startsWith(ROOT) || !existsSync(target)) return href
  const kind = statSync(target).isDirectory() ? "tree" : "blob"
  const anchor = hash === undefined ? "" : `#${hash}`
  return `${REPOSITORY}/${kind}/${BRANCH}/${relative(ROOT, target)}${anchor}`
}

const all = pages()

export default defineConfig({
  title: "atelier",
  description: "A CI engine for monorepos: gate presets on moon, one GitHub check per gate",
  srcDir: ROOT,
  srcExclude: ["**/node_modules/**"],
  rewrites: Object.fromEntries(all.map((page) => [page, rewrite(page)])),
  cleanUrls: true,
  // Pages live outside this folder, so their imports resolve from its node_modules.
  vite: {
    resolve: { alias: [{ find: /^vue(?=$|\/)/, replacement: join(SITE, "node_modules/vue") }] },
    // Mermaid's own chunks reach 674 kB; they load only on a page with a diagram.
    build: { chunkSizeWarningLimit: 700 },
  },
  lastUpdated: true,
  themeConfig: {
    nav: [
      { text: "Design", link: "https://github.com/wrkhse/atelier/issues/85" },
      { text: "Slices", link: "/docs/plans/" },
      { text: "Plan", link: "https://github.com/wrkhse/atelier/issues/1" },
    ],
    sidebar: sidebar(all),
    outline: [2, 3],
    search: { provider: "local" },
    socialLinks: [{ icon: "github", link: REPOSITORY }],
    editLink: { pattern: `${REPOSITORY}/edit/${BRANCH}/:path` },
  },
  markdown: {
    config(md) {
      const fence = md.renderer.rules.fence
      md.renderer.rules.fence = (tokens, at, options, env, self) => {
        const token = tokens[at]
        if (token.info.trim() !== "mermaid") return fence?.(tokens, at, options, env, self) ?? ""
        // v-pre keeps Vue from reading {{ }} in a diagram as a template.
        return `<pre class="mermaid" v-pre>${md.utils.escapeHtml(token.content)}</pre>`
      }
      md.core.ruler.push("source-links", (state) => {
        const from = state.env.path as string | undefined
        if (!from) return
        for (const token of state.tokens.flatMap((block) => block.children ?? [])) {
          const href = token.type === "link_open" ? token.attrGet("href") : null
          if (href) token.attrSet("href", sourceLink(href, from))
        }
      })
    },
  },
})
