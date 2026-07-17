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
  {
    rules: {
      'ts/no-use-before-define': 'off',
      'vue/custom-event-name-casing': 'off',
      'vue/no-unused-refs': 'off',
      'vue-a11y/click-events-have-key-events': 'off',
      'vue-a11y/label-has-for': 'off',
      'vue-a11y/mouse-events-have-key-events': 'off',
      'vue-a11y/no-redundant-roles': 'off',
      'vue-a11y/no-static-element-interactions': 'off',
    },
  },
  {
    files: ['src/api/**/*.ts'],
    rules: {
      'no-restricted-imports': [
        'error',
        {
          patterns: [
            {
              group: ['@/layout/**', '@/router/**', '@/stores/**', '@/views/**'],
              message: 'API transport 和 adapter 不能反向依赖 UI、路由或状态层。',
            },
          ],
        },
      ],
    },
  },
  {
    files: ['src/composables/**/*.ts', 'src/layout/**/*.vue', 'src/stores/**/*.ts', 'src/views/**'],
    rules: {
      'no-restricted-imports': [
        'error',
        {
          paths: [
            {
              name: 'axios',
              message: 'HTTP transport 只能由 src/api 持有。',
            },
          ],
        },
      ],
    },
  },
)
