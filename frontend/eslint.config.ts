import antfu from '@antfu/eslint-config'

const templateStylePlugin = {
  rules: {
    'single-line-simple-interpolation': {
      meta: {
        type: 'layout',
        fixable: 'whitespace',
        schema: [],
        messages: {
          expectedSingleLine: 'Expected a simple mustache interpolation to stay on one line.',
        },
      },
      create(context) {
        const { parserServices } = context.sourceCode
        if (!parserServices.defineTemplateBodyVisitor)
          return {}

        return parserServices.defineTemplateBodyVisitor({
          'VExpressionContainer[expression!=null]': function (node) {
            if (node.parent.type !== 'VElement')
              return

            const expression = node.expression
            if (
              expression.loc.start.line !== expression.loc.end.line
              || node.loc.start.line === node.loc.end.line
            ) {
              return
            }

            context.report({
              node,
              messageId: 'expectedSingleLine',
              fix(fixer) {
                return fixer.replaceText(node, `{{ ${context.sourceCode.getText(expression)} }}`)
              },
            })
          },
        })
      },
    },
  },
}

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
    files: ['**/*.vue'],
    plugins: {
      'template-style': templateStylePlugin,
    },
    rules: {
      'template-style/single-line-simple-interpolation': 'error',
      'vue/html-closing-bracket-newline': ['error', {
        singleline: 'never',
        multiline: 'always',
      }],
      'vue/multiline-html-element-content-newline': ['error', {
        ignores: ['pre', 'textarea'],
        ignoreWhenEmpty: true,
        allowEmptyLines: false,
      }],
      'vue/mustache-interpolation-spacing': ['error', 'always'],
    },
  },
)
