<script setup lang="ts">
import type { JsonSchema } from '../utils/model'

import { BaseEmpty, BaseFormItem, BaseInput, BaseTag } from '@codex-proxy/ui'

import { computed, nextTick, shallowRef, useTemplateRef, watch } from 'vue'
import SchemaForm from '@/components/SchemaForm/index.vue'
import { jsonObjectError, parseJsonObject } from '@/utils/jsonObject'
import PluginHelpPopover from './PluginHelpPopover.vue'

const props = withDefaults(defineProps<{
  schema: Record<string, unknown>
  secretFields: string[]
  existingSecretFields?: string[]
  secretMode?: 'preserve' | 'replace' | 'clear'
  disabled?: boolean
}>(), {
  existingSecretFields: () => [],
  secretMode: 'replace',
  disabled: false,
})

const emit = defineEmits<{
  validityChange: [valid: boolean]
}>()

const configuration = defineModel<Record<string, unknown>>('configuration', { required: true })
const secrets = defineModel<Record<string, string>>('secrets', { required: true })
const configurationText = shallowRef('{}')
const schemaForm = useTemplateRef<{ focusField: (key?: string) => Promise<boolean> }>('schemaForm')
const secretFieldsRoot = useTemplateRef<HTMLElement>('secretFieldsRoot')
const configurationError = computed(() => jsonObjectError(configurationText.value, 48 * 1024))
const configurationSchema = computed(() => {
  const schema = { ...props.schema } as JsonSchema
  if (schema.properties) {
    schema.properties = Object.fromEntries(
      Object.entries(schema.properties).filter(([name]) => !props.secretFields.includes(name)),
    )
  }
  if (schema.required)
    schema.required = schema.required.filter(name => !props.secretFields.includes(name))
  return schema as Record<string, unknown>
})
const secretSchemas = computed(() => {
  const properties = (props.schema as JsonSchema).properties ?? {}
  const required = (props.schema as JsonSchema).required ?? []
  return props.secretFields.map(name => ({
    name,
    schema: properties[name] ?? { type: 'string' },
    required: required.includes(name),
  }))
})
const requiredConfigurationFields = computed(() => (configurationSchema.value as JsonSchema).required ?? [])
const missingConfigurationFields = computed(() => requiredConfigurationFields.value.filter(name =>
  !Object.hasOwn(configuration.value, name),
))
const configurationFieldErrors = computed(() => Object.fromEntries(
  missingConfigurationFields.value.map(name => [name, '请填写必填配置']),
))
const requiredSecretFields = computed(() => secretSchemas.value.filter(field => field.required))
const missingSecretFields = computed(() => requiredSecretFields.value.filter((field) => {
  if (props.secretMode === 'clear')
    return true
  if (props.secretMode === 'preserve')
    return !props.existingSecretFields.includes(field.name)
  return !valuePresent(secrets.value[field.name])
}))
const secretModeError = computed(() => {
  if (props.secretMode === 'preserve' && props.existingSecretFields.some(name => !props.secretFields.includes(name)))
    return '目标版本不支持部分已保存的敏感配置，请选择替换或清除'
  if (!missingSecretFields.value.length || props.secretMode === 'replace')
    return ''
  const labels = missingSecretFields.value.map(field => field.schema.title || field.name).join('、')
  return props.secretMode === 'clear'
    ? `必填敏感配置不能清除：${labels}`
    : `没有可保留的必填敏感配置：${labels}`
})
const hasConfiguration = computed(() => {
  const properties = (configurationSchema.value as JsonSchema).properties
  return Boolean(properties && Object.keys(properties).length > 0)
    || (configurationSchema.value as JsonSchema).additionalProperties !== false
    || Object.keys(configuration.value).length > 0
})
const hasFields = computed(() => hasConfiguration.value || props.secretFields.length > 0)

function setSecret(name: string, value: unknown) {
  const next = { ...secrets.value }
  if (typeof value !== 'string' || !value) {
    delete next[name]
    secrets.value = next
    return
  }
  // 计算属性保留 __proto__ 等合法 JSON 字段，不触发普通对象的原型 setter
  secrets.value = { ...next, [name]: value }
}

function valuePresent(value: unknown) {
  return value !== undefined && value !== null && value !== ''
}

const valid = computed(() => !configurationError.value
  && !secretModeError.value
  && !missingConfigurationFields.value.length
  && !missingSecretFields.value.length)
const validationMessage = computed(() => secretModeError.value
  || configurationError.value
  || (missingSecretFields.value.length ? '请填写必填敏感配置' : '请检查基本设置'))

watch(valid, value => emit('validityChange', value), { immediate: true })

watch(
  configuration,
  (value) => {
    const next = JSON.stringify(value, null, 2)
    if (configurationText.value !== next)
      configurationText.value = next
  },
  { immediate: true },
)

watch(configurationText, (text) => {
  if (!jsonObjectError(text, 48 * 1024))
    configuration.value = parseJsonObject(text, 48 * 1024)
})

async function focusInvalid() {
  if (secretModeError.value)
    return 'secretMode' as const
  if (configurationError.value) {
    await schemaForm.value?.focusField()
    return 'configuration' as const
  }
  if (missingConfigurationFields.value.length) {
    await schemaForm.value?.focusField(missingConfigurationFields.value[0])
    return 'configuration' as const
  }
  if (!missingSecretFields.value.length)
    return null
  if (props.secretMode !== 'replace')
    return 'secretMode' as const

  await nextTick()
  const missingName = missingSecretFields.value[0]?.name
  const container = [...(secretFieldsRoot.value?.querySelectorAll<HTMLElement>('[data-secret-field]') ?? [])]
    .find(element => element.dataset.secretField === missingName)
  container?.querySelector<HTMLElement>('input:not([type="hidden"]), textarea, button, [tabindex]:not([tabindex="-1"])')?.focus()
  return 'secret' as const
}

defineExpose({ focusInvalid, validationMessage })
</script>

<template>
  <BaseEmpty
    v-if="!hasFields"
    title="无需填写插件参数"
    size="sm"
    surface="inset"
    class="min-h-40 content-center"
  />
  <div v-else class="grid gap-5">
    <SchemaForm
      v-if="hasConfiguration"
      ref="schemaForm"
      v-model="configurationText"
      :schema="configurationSchema"
      :disabled="disabled"
      :maximum-bytes="48 * 1024"
      :field-errors="configurationFieldErrors"
    />

    <template v-if="secretFields.length > 0">
      <div class="flex min-w-0 flex-wrap items-center gap-2">
        <strong class="text-cp leading-none text-cp-text">敏感配置</strong>
        <PluginHelpPopover label="敏感配置说明">
          敏感配置单独保存，不回显已有值，选择保留时不会更改，选择替换时以本次填写的全部值覆盖
        </PluginHelpPopover>
        <BaseTag v-if="existingSecretFields.length > 0" type="success" size="sm">
          已保存 {{ existingSecretFields.length }} 项
        </BaseTag>
      </div>
      <p v-if="secretMode === 'clear'" class="m-0 text-cp-xs text-cp-warning-text">
        保存时清除全部敏感配置
      </p>
      <div ref="secretFieldsRoot" class="contents">
        <BaseFormItem
          v-for="field in secretMode === 'replace' ? secretSchemas : []"
          :key="field.name"
          :label="field.schema.title || field.name"
          :required="field.required"
          :data-secret-field="field.name"
        >
          <template v-if="field.schema.description" #label-extra>
            <PluginHelpPopover :label="`${field.schema.title || field.name}说明`">
              {{ field.schema.description }}
            </PluginHelpPopover>
          </template>
          <BaseInput
            :model-value="secrets[field.name] ?? ''"
            type="password"
            autocomplete="new-password"
            :disabled="disabled"
            :aria-label="field.schema.title || field.name"
            :aria-invalid="field.required && !valuePresent(secrets[field.name])"
            placeholder="输入新值"
            @update:model-value="setSecret(field.name, $event)"
          />
        </BaseFormItem>
      </div>
    </template>
  </div>
</template>
