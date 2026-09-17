import { copyFile, writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { defineConfig, type DefaultTheme } from 'vitepress'

const repository = 'https://github.com/JetSquirrel/DuckLocal'
const pages = [
  'index',
  'getting-started',
  'data-sources',
  's3',
  'sql-editor',
  'schema-and-history',
  'results-and-charts',
  'settings-and-data',
  'development',
]

function sidebar(zh = false): DefaultTheme.SidebarItem[] {
  const prefix = zh ? '/zh/' : '/'
  const item = (page: string, english: string, chinese: string) => ({
    text: zh ? chinese : english,
    link: `${prefix}${page}`,
  })
  return [
    {
      text: zh ? '入门' : 'Getting started',
      items: [item('getting-started', 'Quick start', '快速上手')],
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
      text: zh ? '开发' : 'Development',
      items: [
        item('cli', 'AI CLI and official skill', 'AI CLI 与官方 skill'),
        item('development', 'Development guide', '开发指南'),
      ],
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
  head: [['link', { rel: 'icon', type: 'image/png', href: '/DuckLocal/favicon.png' }]],
  locales: {
    root: {
      label: 'English',
      lang: 'en',
      themeConfig: {
        nav: [
          { text: 'Guide', link: '/getting-started' },
          { text: 'Development', link: '/development' },
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
          { text: '指南', link: '/zh/getting-started' },
          { text: '开发', link: '/zh/development' },
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
