<script setup lang="ts">
import type { InputField } from './schema'

import { ZForm, ZFormItem, ZIconButton, ZInput, ZPopover, ZSelect, ZTextarea } from '@codex-proxy/ui'
import { Braces, Info, List } from '@lucide/vue'
import { computed, nextTick, shallowRef, useTemplateRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { parseJsonObject } from '@/utils/data'
import { inputFields } from './schema'

const props = defineProps<{
  schema: Record<string, unknown>
  disabled: boolean
  maximumBytes?: number
  fieldErrors?: Record<string, string>
}>()

const { t } = useI18n({ useScope: 'global' })

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
  return field.type === 'boolean'
    ? typeof item !== 'boolean'
    : typeof item !== 'string'
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
  return value.value && Object.hasOwn(value.value, key)
    ? value.value[key]
    : undefined
}

function fieldOptions(field: InputField) {
  const options = field.values
    ? field.values.map((item, index) => ({
        value: String(index),
        label: String(item),
      }))
    : [
        {
          value: 'true',
          label: t('common.values.yes'),
        },
        {
          value: 'false',
          label: t('common.values.no'),
        },
      ]
  return [
    {
      value: '',
      label: t('common.values.unspecified'),
    },
    ...options,
  ]
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
    next = Number.isFinite(Number(input))
      ? Number(input)
      : input
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
    ? [
        ...(root.value?.querySelectorAll<HTMLElement>('[data-schema-control]') ?? []),
      ]
        .find(element => element.dataset.schemaControl === key)
    : root.value?.querySelector<HTMLElement>('[data-schema-json-control], textarea')
  control?.focus()
  return Boolean(control)
}

defineExpose({ focusField })
</script>

<template>
  <div v-if="!empty" ref="root" class="grid gap-3">
    <ZForm v-if="showJson">
      <ZFormItem :label="t('common.form.inputPlaceholder')" :error="error">
        <template #extra>
          <ZIconButton v-if="fields !== null" :aria-label="t('common.form.useForm')" size="small" variant="solid" :disabled="disabled || Boolean(error) || hasExtra || incompatibleValue" @click="jsonMode = false">
            <List class="size-4" />
          </ZIconButton>
        </template>
        <ZTextarea v-model="text" :rows="8" :disabled="disabled" spellcheck="false" autocomplete="off" data-schema-json-control />
      </ZFormItem>
    </ZForm>
    <ZForm v-else>
      <ZFormItem
        v-for="(field, index) in fields"
        :key="field.key"
        :label="field.label"
        :required="field.required"
        :error="fieldErrors?.[field.key]"
      >
        <template v-if="field.description" #label-extra>
          <ZPopover trigger="hover-click" placement="top-start">
            <template #reference>
              <ZIconButton :aria-label="t('common.form.fieldHelp', { label: field.label })" class="size-3.5!" :title="undefined">
                <Info class="size-3.5" />
              </ZIconButton>
            </template>
            <p class="m-0 max-w-72 p-3 text-cp-xs leading-relaxed text-cp-text-secondary">
              {{ field.description }}
            </p>
          </ZPopover>
        </template>
        <template v-if="index === 0" #extra>
          <ZIconButton :aria-label="t('common.form.editJson')" size="small" variant="solid" :disabled="disabled" @click="jsonMode = true">
            <Braces class="size-4" />
          </ZIconButton>
        </template>
        <ZSelect
          v-if="field.values || field.type === 'boolean'"
          :model-value="fieldValue(field)"
          :options="fieldOptions(field)"
          :disabled="disabled"
          :data-schema-control="field.key"
          class="w-full"
          @update:model-value="update(field, $event ?? '')"
        />
        <ZInput
          v-else
          :model-value="fieldValue(field)"
          :type="field.secret ? 'password' : 'text'"
          :inputmode="field.type === 'integer' ? 'numeric' : field.type === 'number' ? 'decimal' : 'text'"
          :disabled="disabled"
          :data-schema-control="field.key"
          autocomplete="off"
          @update:model-value="update(field, $event)"
        />
      </ZFormItem>
    </ZForm>
  </div>
</template>
