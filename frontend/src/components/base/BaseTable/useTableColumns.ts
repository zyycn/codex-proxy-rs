import type { MaybeRefOrGetter } from 'vue'
import type { BaseTableColumn, TableRow } from './columns'
import { useStorage } from '@vueuse/core'
import { computed, toValue } from 'vue'

export interface TableColumnOption {
  key: string
  label: string
  visible: boolean
  disabled: boolean
}

function readVisibility(value: string): Record<string, boolean> {
  try {
    const parsed: unknown = JSON.parse(value)
    if (parsed && typeof parsed === 'object' && !Array.isArray(parsed))
      return Object.fromEntries(Object.entries(parsed).filter(([, visible]) => typeof visible === 'boolean'))
  }
  catch {
    // 损坏的本地偏好回退到列定义，不影响表格展示。
  }
  return {}
}

export function useTableColumns<Row extends TableRow>(
  source: MaybeRefOrGetter<BaseTableColumn<Row>[]>,
  tableId: string,
) {
  // 只保存用户覆盖；新增列继续使用自身默认值，不继承旧列表的隐藏状态。
  const overrides = useStorage<Record<string, boolean>>(
    `codex-proxy:table-columns:${tableId}`,
    {},
    undefined,
    {
      shallow: true,
      writeDefaults: false,
      serializer: { read: readVisibility, write: JSON.stringify },
    },
  )

  const columnStates = computed(() => {
    const states = toValue(source).map((column) => {
      const override = overrides.value[column.key]
      return {
        column,
        visible: column.hideable === false || (typeof override === 'boolean' ? override : !column.defaultHidden),
      }
    })
    // 即使旧偏好隐藏了全部列，也保留一个可操作的表格。
    if (!states.some(state => state.visible) && states[0])
      states[0].visible = true
    return states
  })
  const visibleColumns = computed(() => columnStates.value.filter(state => state.visible).map(state => state.column))
  const columnOptions = computed<TableColumnOption[]>(() => columnStates.value
    .filter(({ column }) => column.label)
    .map(({ column, visible }) => ({
      key: column.key,
      label: column.label!,
      visible,
      disabled: column.hideable === false || (visible && visibleColumns.value.length === 1),
    })))

  function setColumnVisible(key: string, visible: boolean) {
    const option = columnOptions.value.find(option => option.key === key)
    const column = toValue(source).find(column => column.key === key)
    if (!column || !option || option.disabled)
      return

    const next = { ...overrides.value }
    if (visible === !column.defaultHidden)
      delete next[key]
    else next[key] = visible
    overrides.value = next
  }

  function resetColumns() {
    overrides.value = {}
  }

  return { visibleColumns, columnOptions, setColumnVisible, resetColumns }
}
