import antfu from '@antfu/eslint-config'

export default antfu(
  {
    type: 'app',
    ignores: ['dist', 'coverage', 'pnpm-workspace.yaml'],
    formatters: {
      css: true,
      html: true,
      markdown: 'prettier',
    },
    stylistic: {
      indent: 2,
      quotes: 'single',
    },
    typescript: true,
    vue: {
      a11y: true,
    },
  },
)
