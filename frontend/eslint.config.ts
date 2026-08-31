import type { ESLint, Rule } from 'eslint'
import antfu from '@antfu/eslint-config'

type LocatedRuleNode = Rule.Node & {
  loc: NonNullable<Rule.Node['loc']>
}

interface TemplateExpressionContainer {
  type: 'VExpressionContainer'
  parent: { type: string }
  expression: LocatedRuleNode
  loc: NonNullable<Rule.Node['loc']>
  range: NonNullable<Rule.Node['range']>
}

const singleLineSimpleInterpolationRule = {
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
      'VExpressionContainer[expression!=null]': function (node: TemplateExpressionContainer) {
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
            return fixer.replaceTextRange(
              node.range,
              `{{ ${context.sourceCode.getText(expression)} }}`,
            )
          },
        })
      },
    })
  },
} satisfies Rule.RuleModule

const templateStylePlugin = {
  rules: {
    'single-line-simple-interpolation': singleLineSimpleInterpolationRule,
  },
} satisfies ESLint.Plugin

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
