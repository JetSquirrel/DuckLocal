import { copyFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { defineConfig, type DefaultTheme, type HeadConfig } from 'vitepress'

const repository = 'https://github.com/JetSquirrel/DuckLocal'
// Where the site is published: GitHub Pages under the repository's name. Every
// absolute URL the pages hand to crawlers and link previews starts here.
const site = 'https://jetsquirrel.github.io/DuckLocal/'
const ogImage = `${site}og-image.jpg`
const pages = [
  'index',
  'getting-started',
  'data-sources',
  's3',
  'sql-editor',
  'schema-and-history',
  'results-and-charts',
  'settings-and-data',
  'analysis-app',
  'development',
]

// A page's path under the site root, as `cleanUrls: false` publishes it:
// `index.md` → ``, `zh/index.md` → `zh/`, `zh/cli.md` → `zh/cli.html`.
function pageUrl(relativePath: string): string {
  return relativePath.replace(/(^|\/)index\.md$/, '$1').replace(/\.md$/, '.html')
}

function sidebar(zh = false): DefaultTheme.SidebarItem[] {
  const prefix = zh ? '/zh/' : '/'
  const item = (page: string, english: string, chinese: string) => ({
    text: zh ? chinese : english,
    link: `${prefix}${page}`,
  })
  return [
    {
      text: zh ? '入门' : 'Getting started',
      items: [
        item('getting-started', 'Install and first query', '安装与第一条查询'),
        item('tutorial', 'Your first 10 minutes', '10 分钟上手教程'),
        item('troubleshooting', 'Troubleshooting', '常见问题排查'),
      ],
    },
    {
      text: zh ? '使用指南' : 'Guides',
      items: [
        item('data-sources', 'Data sources', '数据源'),
        item('s3', 'S3 and httpfs', 'S3 与 httpfs'),
        item('sql-editor', 'SQL editor', 'SQL 编辑器'),
        item('schema-and-history', 'Schema and history', 'Schema 浏览与历史'),
        item('results-and-charts', 'Results and charts', '结果与图表'),
        item('settings-and-data', 'Settings and app data', '设置与应用数据'),
      ],
    },
    {
      text: zh ? '自动化与扩展' : 'Automate and extend',
      items: [
        item('cli', 'CLI and agent skill', 'CLI 与 agent skill'),
        item('dashboards', 'Dashboards (.dash)', 'Dashboard（.dash）'),
        item('analysis-app', 'Analysis apps', '分析应用'),
      ],
    },
    {
      text: zh ? '参与开发' : 'Contributing',
      items: [item('development', 'Development guide', '开发指南')],
    },
  ]
}

export default defineConfig({
  title: 'DuckLocal',
  description: 'A local-first workspace for querying and exploring your data, built natively on DuckDB.',
  base: '/DuckLocal/',
  outDir: '../target/docs-site',
  cleanUrls: false,
  ignoreDeadLinks: false,
  // Per-page "last updated" dates, which also become the sitemap's lastmod.
  // Needs the full git history; the Pages workflow checks out with depth 0.
  lastUpdated: true,
  sitemap: { hostname: site },
  head: [
    ['link', { rel: 'icon', type: 'image/png', href: '/DuckLocal/favicon.png' }],
    ['meta', { name: 'theme-color', content: '#f2b705' }],
    ['meta', { property: 'og:site_name', content: 'DuckLocal' }],
    ['meta', { property: 'og:type', content: 'website' }],
    ['meta', { property: 'og:image', content: ogImage }],
    ['meta', { property: 'og:image:width', content: '1200' }],
    ['meta', { property: 'og:image:height', content: '675' }],
    ['meta', { property: 'og:image:alt', content: 'CSV, Parquet and DuckDB files, plus S3 storage, feeding one local DuckLocal workspace' }],
    ['meta', { name: 'twitter:card', content: 'summary_large_image' }],
    ['meta', { name: 'twitter:image', content: ogImage }],
  ],
  // What differs page to page: its canonical address, its title and summary
  // for link previews, its other-language twin, and — on the two home pages —
  // a description of the app itself for search results.
  transformHead({ pageData, title, description }) {
    const path = pageUrl(pageData.relativePath)
    const url = `${site}${path}`
    const zh = pageData.relativePath.startsWith('zh/')
    const english = zh ? path.replace(/^zh\//, '') : path
    const chinese = zh ? path : `zh/${path}`
    const head: HeadConfig[] = [
      ['link', { rel: 'canonical', href: url }],
      ['link', { rel: 'alternate', hreflang: 'en', href: `${site}${english}` }],
      ['link', { rel: 'alternate', hreflang: 'zh-CN', href: `${site}${chinese}` }],
      ['link', { rel: 'alternate', hreflang: 'x-default', href: `${site}${english}` }],
      ['meta', { property: 'og:url', content: url }],
      ['meta', { property: 'og:title', content: title }],
      ['meta', { property: 'og:description', content: description }],
      ['meta', { property: 'og:locale', content: zh ? 'zh_CN' : 'en_US' }],
      ['meta', { name: 'twitter:title', content: title }],
      ['meta', { name: 'twitter:description', content: description }],
    ]
    if (pageData.relativePath === 'index.md' || pageData.relativePath === 'zh/index.md') {
      head.push(['script', { type: 'application/ld+json' }, JSON.stringify({
        '@context': 'https://schema.org',
        '@type': 'SoftwareApplication',
        name: 'DuckLocal',
        description,
        url,
        applicationCategory: 'DeveloperApplication',
        operatingSystem: 'macOS 12 or later (Apple silicon)',
        offers: { '@type': 'Offer', price: '0', priceCurrency: 'USD' },
        license: 'https://www.apache.org/licenses/LICENSE-2.0',
        downloadUrl: `${repository}/releases/latest`,
        codeRepository: repository,
        inLanguage: zh ? 'zh-CN' : 'en',
      })])
    }
    return head
  },
  locales: {
    root: {
      label: 'English',
      lang: 'en',
      themeConfig: {
        nav: [
          { text: 'Get started', link: '/getting-started' },
          { text: 'Tutorial', link: '/tutorial' },
          { text: 'CLI', link: '/cli' },
          { text: 'Download', link: `${repository}/releases/latest` },
        ],
        sidebar: sidebar(),
      },
    },
    zh: {
      label: '简体中文',
      lang: 'zh-CN',
      description: '本地优先的数据查询与分析工作台，原生基于 DuckDB。',
      themeConfig: {
        nav: [
          { text: '快速上手', link: '/zh/getting-started' },
          { text: '教程', link: '/zh/tutorial' },
          { text: 'CLI', link: '/zh/cli' },
          { text: '下载', link: `${repository}/releases/latest` },
        ],
        sidebar: sidebar(true),
        outline: { label: '本页目录', level: [2, 3] },
        editLink: { pattern: `${repository}/edit/main/docs/:path`, text: '在 GitHub 上编辑此页' },
        docFooter: { prev: '上一页', next: '下一页' },
        langMenuLabel: '切换语言',
        sidebarMenuLabel: '目录',
        returnToTopLabel: '返回顶部',
        skipToContentLabel: '跳转到内容',
        darkModeSwitchLabel: '外观',
        lightModeSwitchTitle: '切换到浅色主题',
        darkModeSwitchTitle: '切换到深色主题',
        notFound: {
          title: '页面未找到',
          quote: '这个页面不存在，请返回首页或使用搜索查找指南。',
          linkLabel: '返回首页',
          linkText: '返回首页',
        },
        footer: { message: '基于 Apache-2.0 许可开源发布。' },
      },
    },
  },
  themeConfig: {
    logo: { src: '/assets/logo.png', alt: 'DuckLocal' },
    outline: { level: [2, 3] },
    editLink: { pattern: `${repository}/edit/main/docs/:path` },
    socialLinks: [{ icon: 'github', link: repository }],
    footer: { message: 'Released under the Apache-2.0 License.' },
    search: {
      provider: 'local',
      options: {
        locales: {
          zh: {
            translations: {
              button: { buttonText: '搜索', buttonAriaLabel: '搜索文档' },
              modal: {
                displayDetails: '显示详细列表',
                resetButtonTitle: '清除搜索',
                backButtonTitle: '关闭搜索',
                noResultsText: '没有找到相关结果',
                footer: {
                  selectText: '选择',
                  selectKeyAriaLabel: '回车键',
                  navigateText: '切换',
                  navigateUpKeyAriaLabel: '向上箭头',
                  navigateDownKeyAriaLabel: '向下箭头',
                  closeText: '关闭',
                  closeKeyAriaLabel: 'Esc 键',
                },
              },
            },
          },
        },
      },
    },
  },
  async buildEnd(config) {
    await copyFile(resolve(config.srcDir, 'assets/logo.png'), resolve(config.outDir, 'assets/logo.png'))
    await Promise.all(pages.map(async (page) => {
      const target = `${config.site.base}zh/${page === 'index' ? '' : `${page}.html`}`
      await writeFile(resolve(config.outDir, `${page}.zh-CN.html`), `<!doctype html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex">
<title>页面已迁移 · DuckLocal</title>
<link rel="canonical" href="${target}">
<script>location.replace(${JSON.stringify(target)} + location.search + location.hash)</script>
</head>
<body><p>文档已迁移。<a href="${target}">前往新页面</a></p></body>
</html>
`)
    }))
  },
})
