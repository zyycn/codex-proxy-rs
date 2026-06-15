import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { test } from 'node:test'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const tablePath = resolve(root, 'src/components/base/BaseTable.vue')

test('BaseTable uses semantic table elements instead of div grid rows', async () => {
  const source = await readFile(tablePath, 'utf8')

  for (const token of [
    '<table',
    '<colgroup>',
    '<col',
    '<thead>',
    '<tbody>',
    '<tr',
    '<th',
      '<td',
      'table-fixed',
      ':style="{ width: \'100%\' }"',
    ]) {
    assert.match(source, new RegExp(token.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))
  }

  assert.doesNotMatch(source, /gridTemplateColumns/)
  assert.doesNotMatch(source, /class="grid min-w-\[620px\]/)
  assert.doesNotMatch(source, /calc\(/)
})

test('BaseCard is the shared card surface primitive', async () => {
  const source = await readFile(resolve(root, 'src/components/base/BaseCard.vue'), 'utf8')

  for (const token of [
    'as?: keyof HTMLElementTagNameMap',
    'const props = withDefaults',
    '<component',
    ':is="props.as"',
    'rounded-[var(--cp-card-radius)]',
    'shadow-[var(--cp-shadow-card)]',
    'padded ?',
  ]) {
    assert.match(source, new RegExp(token.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))
  }
})
