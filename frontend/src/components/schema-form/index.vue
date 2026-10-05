<script setup lang="ts">
import type { InputField } from './schema'
import { BaseForm, BaseFormItem, BaseIconButton, BaseInput, BasePopover, BaseSelect, BaseTextarea } from '@codex-proxy/ui'
import { Braces, Info, List } from '@lucide/vue'
import { computed, nextTick, shallowRef, useTemplateRef } from 'vue'
import { parseJsonObject } from '@/utils/data'
import { inputFields } from './schema'

const props = defineProps<{
  schema: Record<string, unknown>
  disabled: boolean
  maximumBytes?: number
  fieldErrors?: Record<string, string>
}>()
const text = defineModel<string>({ required: true })
const jsonMode = shallowRef(false)
const root = useTemplateRef<HTMLElement>('root')
const parsed = computed(() => parseJsonObject(text.value, props.maximumBytes))
const error = computed(() => parsed.value.error)
const value = computed(() => parsed.value.value)
const fields = computed(() => inputFields(props.schema))
const hasExtra = computed(() => Object.keys(value.value ?? {}).some(key => !fields.value?.some(field => field.key === key)))
const incompatibleValue = computed(() => fields.value?.some((field) => {
  const item = inputValue(field.key)
  if (item === undefined)
    return false
  if (field.values)
    return !field.values.some(option => option === item)
  if (field.type === 'number' || field.type === 'integer')
    return typeof item !== 'number' && typeof item !== 'string'
  return field.type === 'boolean' ? typeof item !== 'boolean' : typeof item !== 'string'
}) ?? false)
const showJson = computed(() => jsonMode.value || fields.value === null || hasExtra.value || incompatibleValue.value || Boolean(error.value))
const empty = computed(() => fields.value?.length === 0 && props.schema.additionalProperties === false && !showJson.value)

function fieldValue(field: InputField) {
  const item = inputValue(field.key)
  if (item === undefined)
    return ''
  if (field.values)
    return String(field.values.findIndex(entry => entry === item))
  return String(item)
}

function inputValue(key: string) {
  return value.value && Object.hasOwn(value.value, key) ? value.value[key] : undefined
}

function fieldOptions(field: InputField) {
  const options = field.values
    ? field.values.map((item, index) => ({ value: String(index), label: String(item) }))
    : [{ value: 'true', label: '是' }, { value: 'false', label: '否' }]
  return [{ value: '', label: '未填写' }, ...options]
}

function update(field: InputField, input: string) {
  let next: unknown
  if (input === '' && (field.values || field.type !== 'string')) {
    next = undefined
  }
  else if (field.values) {
    next = field.values[Number(input)]
  }
  else if (field.type === 'boolean') {
    next = input === 'true'
  }
  else if (field.type === 'number' || field.type === 'integer') {
    // 不把尚未输入完整的数字静默替换成 0，保留原值供服务端校验。
    next = Number.isFinite(Number(input)) ? Number(input) : input
  }
  else {
    next = input
  }
  // 计算属性保留合法的特殊字段名，不触发普通对象的原型 setter。
  text.value = JSON.stringify({ ...value.value, [field.key]: next }, null, 2)
}

async function focusField(key?: string) {
  const editableField = key && fields.value?.some(field => field.key === key)
  if (editableField && jsonMode.value && !error.value && !hasExtra.value && !incompatibleValue.value)
    jsonMode.value = false
  await nextTick()

  const control = editableField && !showJson.value
    ? [...(root.value?.querySelectorAll<HTMLElement>('[data-schema-control]') ?? [])]
        .find(element => element.dataset.schemaControl === key)
    : root.value?.querySelector<HTMLElement>('[data-schema-json-control], textarea')
  control?.focus()
  return Boolean(control)
}

defineExpose({ focusField })
</script>

<template>
  <div v-if="!empty" ref="root" class="grid gap-3">
    <BaseForm v-if="showJson">
      <BaseFormItem label="输入内容" :error="error">
        <template #extra>
          <BaseIconButton v-if="fields !== null" label="使用表单" size="sm" variant="secondary" :disabled="disabled || Boolean(error) || hasExtra || incompatibleValue" @click="jsonMode = false">
            <List class="size-4" />
          </BaseIconButton>
        </template>
        <BaseTextarea v-model="text" :rows="8" :disabled="disabled" spellcheck="false" autocomplete="off" data-schema-json-control />
      </BaseFormItem>
    </BaseForm>
    <BaseForm v-else>
      <BaseFormItem
        v-for="(field, index) in fields"
        :key="field.key"
        :label="field.label"
        :required="field.required"
        :error="fieldErrors?.[field.key]"
      >
        <template v-if="field.description" #label-extra>
          <BasePopover trigger="hover-click" placement="top-start">
            <template #trigger>
              <BaseIconButton :label="`${field.label}说明`" class="size-3.5!" :title="undefined">
                <Info class="size-3.5" />
              </BaseIconButton>
            </template>
            <p class="m-0 max-w-72 p-3 text-cp-xs leading-relaxed text-cp-text-secondary">
              {{ field.description }}
            </p>
          </BasePopover>
        </template>
        <template v-if="index === 0" #extra>
          <BaseIconButton label="编辑 JSON" size="sm" variant="secondary" :disabled="disabled" @click="jsonMode = true">
            <Braces class="size-4" />
          </BaseIconButton>
        </template>
        <BaseSelect
          v-if="field.values || field.type === 'boolean'"
          :model-value="fieldValue(field)"
          :options="fieldOptions(field)"
          :disabled="disabled"
          :data-schema-control="field.key"
          class="w-full"
          @update:model-value="update(field, $event)"
        />
        <BaseInput
          v-else
          :model-value="fieldValue(field)"
          :type="field.secret ? 'password' : 'text'"
          :inputmode="field.type === 'integer' ? 'numeric' : field.type === 'number' ? 'decimal' : 'text'"
          :disabled="disabled"
          :data-schema-control="field.key"
          autocomplete="off"
          @update:model-value="update(field, $event)"
        />
      </BaseFormItem>
    </BaseForm>
  </div>
</template>
