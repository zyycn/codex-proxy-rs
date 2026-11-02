<script setup lang="ts">
import type { JsonSchema } from '../utils/model'

import { ZEmpty, ZFormItem, ZIconButton, ZInput } from '@codex-proxy/ui'
import { RotateCcw, X } from '@lucide/vue'
import { computed, nextTick, shallowRef, useTemplateRef, watch } from 'vue'

import { useI18n } from 'vue-i18n'

import SchemaForm from '@/components/schema-form/index.vue'
import { parseJsonObject } from '@/utils/data'
import { PLUGIN_CONFIGURATION_MAX_BYTES } from '../constants'
import PluginHelpPopover from './PluginHelpPopover.vue'

const props = withDefaults(defineProps<{
  schema: Record<string, unknown>
  secretFields: string[]
  existingSecretFields?: string[]
  disabled?: boolean
}>(), {
  existingSecretFields: () => [],
  disabled: false,
})

const emit = defineEmits<{
  validityChange: [valid: boolean]
}>()

const { t } = useI18n({ useScope: 'global' })

const configuration = defineModel<Record<string, unknown>>('configuration', { required: true })
const secrets = defineModel<Record<string, string | null>>('secrets', { required: true })
const configurationText = shallowRef('{}')
const schemaForm = useTemplateRef<{ focusField: (key?: string) => Promise<boolean> }>('schemaForm')
const secretFieldsRoot = useTemplateRef<HTMLElement>('secretFieldsRoot')
const parsedConfiguration = computed(() => parseJsonObject(configurationText.value, PLUGIN_CONFIGURATION_MAX_BYTES))
const configurationError = computed(() => parsedConfiguration.value.error)
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
  return [
    ...new Set([
      ...props.secretFields,
      ...props.existingSecretFields,
    ]),
  ].map(name => ({
    name,
    schema: properties[name] ?? { type: 'string' },
    required: props.secretFields.includes(name) && required.includes(name),
    declared: props.secretFields.includes(name),
    saved: props.existingSecretFields.includes(name),
  }))
})
const requiredConfigurationFields = computed(() => (configurationSchema.value as JsonSchema).required ?? [])
const missingConfigurationFields = computed(() => requiredConfigurationFields.value.filter(name =>
  !Object.hasOwn(configuration.value, name),
))
const configurationFieldErrors = computed(() => Object.fromEntries(
  missingConfigurationFields.value.map(name => [
    name,
    t('plugins.configuration.fillInRequiredConfiguration'),
  ]),
))
const requiredSecretFields = computed(() => secretSchemas.value.filter(field => field.required))
const missingSecretFields = computed(() => requiredSecretFields.value.filter(field => !secretPresent(field.name)))
const incompatibleSecretFields = computed(() => secretSchemas.value.filter(field => !field.declared && secretPresent(field.name)))
const secretError = computed(() => incompatibleSecretFields.value.length
  ? t('plugins.configuration.incompatibleSecrets')
  : '')

function secretChange(name: string) {
  return Object.hasOwn(secrets.value, name)
    ? secrets.value[name]
    : undefined
}

function secretPresent(name: string) {
  const change = secretChange(name)
  return change === undefined
    ? props.existingSecretFields.includes(name)
    : valuePresent(change)
}

function secretPlaceholder(name: string) {
  if (secretChange(name) === null)
    return t('plugins.configuration.secretPendingClear')
  if (!props.secretFields.includes(name))
    return t('plugins.configuration.secretNotDeclared')
  return props.existingSecretFields.includes(name)
    ? t('plugins.configuration.emptySecretPlaceholder')
    : t('plugins.configuration.enterNewValue')
}

function toggleSecretClear(name: string) {
  if (secretChange(name) === null) {
    const next = { ...secrets.value }
    delete next[name]
    secrets.value = next
  }
  else {
    secrets.value = { ...secrets.value, [name]: null }
  }
}

const hasConfiguration = computed(() => {
  const properties = (configurationSchema.value as JsonSchema).properties
  return Boolean(properties && Object.keys(properties).length > 0)
    || (configurationSchema.value as JsonSchema).additionalProperties !== false
    || Object.keys(configuration.value).length > 0
})
const hasSecrets = computed(() => props.secretFields.length > 0 || props.existingSecretFields.length > 0)
const hasFields = computed(() => hasConfiguration.value || hasSecrets.value)

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
  && !secretError.value
  && !missingConfigurationFields.value.length
  && !missingSecretFields.value.length)
const validationMessage = computed(() => secretError.value
  || configurationError.value
  || (missingSecretFields.value.length
    ? t('plugins.configuration.secretsRequired')
    : t('plugins.configuration.checkBasicSettings')))

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

watch(parsedConfiguration, ({ value }) => {
  if (value)
    configuration.value = value
})

async function focusInvalid() {
  if (configurationError.value) {
    await schemaForm.value?.focusField()
    return 'configuration' as const
  }
  if (missingConfigurationFields.value.length) {
    await schemaForm.value?.focusField(missingConfigurationFields.value[0])
    return 'configuration' as const
  }
  const missingName = missingSecretFields.value[0]?.name ?? incompatibleSecretFields.value[0]?.name
  if (!missingName)
    return null
  await nextTick()
  const container = [
    ...(secretFieldsRoot.value?.querySelectorAll<HTMLElement>('[data-secret-field]') ?? []),
  ]
    .find(element => element.dataset.secretField === missingName)
  container?.querySelector<HTMLInputElement>('input')?.focus()
  return 'secret' as const
}

defineExpose({ focusInvalid, validationMessage })
</script>

<template>
  <ZEmpty
    v-if="!hasFields"
    :title="t('plugins.configuration.noParameters')"
    size="small"
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
      :maximum-bytes="PLUGIN_CONFIGURATION_MAX_BYTES"
      :field-errors="configurationFieldErrors"
    />

    <section v-if="hasSecrets" class="grid gap-4 rounded-z bg-z-fill-alter p-4" :aria-label="t('plugins.configuration.sensitiveConfiguration')">
      <h3 class="m-0 text-z-lg leading-none font-heavy text-z-text">
        {{ t('plugins.configuration.sensitiveConfiguration') }}
      </h3>
      <p v-if="secretError" role="alert" class="m-0 text-z-xs leading-relaxed text-z-warning-text">
        {{ secretError }}
      </p>
      <div ref="secretFieldsRoot" class="grid items-start gap-4 sm:grid-cols-2">
        <ZFormItem
          v-for="field in secretSchemas"
          :key="field.name"
          :label="field.schema.title || field.name"
          :required="field.required"
          :data-secret-field="field.name"
        >
          <template v-if="field.schema.description" #label-extra>
            <PluginHelpPopover :label="t('plugins.configuration.about', { name: field.schema.title || field.name })">
              {{ field.schema.description }}
            </PluginHelpPopover>
          </template>
          <ZInput
            :model-value="secretChange(field.name) ?? ''"
            type="password"
            autocomplete="new-password"
            clearable
            :disabled="disabled"
            :readonly="!field.declared"
            :aria-label="field.schema.title || field.name"
            :aria-invalid="field.required && !secretPresent(field.name)"
            :placeholder="secretPlaceholder(field.name)"
            @update:model-value="setSecret(field.name, $event)"
          >
            <template v-if="field.saved && !secretChange(field.name)" #suffix>
              <ZIconButton
                variant="link"
                class="size-4!"
                :disabled="disabled"
                :aria-label="secretChange(field.name) === null ? t('plugins.configuration.undoClearSecret', { name: field.schema.title || field.name }) : t('plugins.configuration.clearSecret', { name: field.schema.title || field.name })"
                @click="toggleSecretClear(field.name)"
              >
                <RotateCcw v-if="secretChange(field.name) === null" class="size-3.5" />
                <X v-else class="size-3.5" />
              </ZIconButton>
            </template>
          </ZInput>
          <p v-if="field.required && !secretPresent(field.name)" role="alert" class="mt-1 text-z-xs text-z-warning-text">
            {{ t('plugins.configuration.secretsRequired') }}
          </p>
        </ZFormItem>
      </div>
    </section>
  </div>
</template>
