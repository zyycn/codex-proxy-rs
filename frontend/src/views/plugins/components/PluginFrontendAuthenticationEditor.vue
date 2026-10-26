<script setup lang="ts">
import type {
  ApiKey,
  PluginCapabilityBinding,
  PluginContribution,
  PluginFailurePolicy,
  PluginFrontendIdentityBinding,
} from '@/api'

import { ZButton, ZFormItem, ZIconButton, ZInput, ZSegmented, ZSelect, ZSwitch } from '@codex-proxy/ui'
import { Plus, RefreshCw, Trash2 } from '@lucide/vue'

import { computed, shallowRef, watch } from 'vue'

import { useI18n } from 'vue-i18n'
import { getApiKeys } from '@/api'
import { notification as ZNotification } from '@/utils/notification'
import { errorMessage } from '@/utils/operation'
import PluginHelpPopover from './PluginHelpPopover.vue'

const props = withDefaults(defineProps<{
  contribution: PluginContribution
  active?: boolean
  disabled?: boolean
}>(), {
  active: false,
  disabled: false,
})

const emit = defineEmits<{
  validityChange: [valid: boolean]
}>()

const { t } = useI18n({ useScope: 'global' })

const binding = defineModel<PluginCapabilityBinding | null>({ required: true })
const clientKeys = shallowRef<ApiKey[]>([])
const loadingKeys = shallowRef(false)
const keysFailed = shallowRef(false)
let keyLoadController: AbortController | null = null

const fallbackOptions = computed(() => ([
  {
    label: t('plugins.frontendAuth.exclusiveCertification'),
    value: 'reject',
  },
  {
    label: t('plugins.frontendAuth.fallbackApiKeyIfNoMatch'),
    value: 'delegate',
  },
]))

const keyOptions = computed(() => {
  const options = clientKeys.value.map(key => ({
    label: `${key.name || key.prefix}${key.enabled
      ? ''
      : t('plugins.frontendAuth.disabled')}`,
    value: key.id,
    description: `${key.prefix} · ${key.id}`,
  }))
  const knownIds = new Set(clientKeys.value.map(key => key.id))
  for (const identity of binding.value?.identityBindings ?? []) {
    if (identity.clientKeyId && !knownIds.has(identity.clientKeyId)) {
      options.push({
        label: t('plugins.frontendAuth.configuredUnavailable'),
        value: identity.clientKeyId,
        description: identity.clientKeyId,
      })
      knownIds.add(identity.clientKeyId)
    }
  }
  return options
})

const validationError = computed(() => {
  const value = binding.value
  if (!value)
    return ''
  if (
    value.contribution !== props.contribution.id
    || value.stage !== 'authentication'
    || !props.contribution.stages.includes(value.stage)
    || (value.failurePolicy !== 'reject' && value.failurePolicy !== 'delegate')
  ) {
    return t('plugins.frontendAuth.invalidBinding')
  }
  if (
    value.providerIds.length
    || value.models.length
    || value.clientKeyIds.length
    || value.accountGroupIds.length
  ) {
    return t('plugins.frontendAuth.unsupportedScope')
  }
  if (value.identityBindings.length === 0)
    return t('plugins.frontendAuth.addAtLeastOneIdentityMapping')
  if (value.identityBindings.length > 256)
    return t('plugins.frontendAuth.mappingLimit')

  const principals = new Set<string>()
  for (const identity of value.identityBindings) {
    if (
      !identity.principal
      || identity.principal !== identity.principal.trim()
      || new TextEncoder().encode(identity.principal).byteLength > 256
      || /\p{Cc}/u.test(identity.principal)
    ) {
      return t('plugins.frontendAuth.invalidIdentity')
    }
    if (principals.has(identity.principal))
      return t('plugins.frontendAuth.duplicateIdentity')
    if (!identity.clientKeyId)
      return t('plugins.frontendAuth.keyRequired')
    principals.add(identity.principal)
  }
  return ''
})

function enableAuthentication(enabled: boolean) {
  binding.value = enabled
    ? {
        contribution: props.contribution.id,
        stage: 'authentication',
        order: 0,
        failurePolicy: 'reject',
        providerIds: [],
        models: [],
        clientKeyIds: [],
        accountGroupIds: [],
        identityBindings: [
          { principal: '', clientKeyId: '' },
        ],
      }
    : null
}

function setFailurePolicy(value: string) {
  if (!binding.value || (value !== 'reject' && value !== 'delegate'))
    return
  binding.value = { ...binding.value, failurePolicy: value as PluginFailurePolicy }
}

function addIdentity() {
  if (!binding.value || binding.value.identityBindings.length >= 256)
    return
  binding.value = {
    ...binding.value,
    identityBindings: [
      ...binding.value.identityBindings,
      { principal: '', clientKeyId: '' },
    ],
  }
}

function updateIdentity(index: number, update: Partial<PluginFrontendIdentityBinding>) {
  if (!binding.value)
    return
  binding.value = {
    ...binding.value,
    identityBindings: binding.value.identityBindings.map((identity, identityIndex) =>
      identityIndex === index
        ? {
            ...identity,
            ...update,
          }
        : identity,
    ),
  }
}

function removeIdentity(index: number) {
  if (!binding.value)
    return
  binding.value = {
    ...binding.value,
    identityBindings: binding.value.identityBindings.filter((_, identityIndex) => identityIndex !== index),
  }
}

async function loadClientKeys() {
  keyLoadController?.abort()
  const controller = new AbortController()
  keyLoadController = controller
  loadingKeys.value = true
  keysFailed.value = false
  try {
    const items: ApiKey[] = []
    const seenCursors = new Set<string>()
    let cursor: string | undefined
    do {
      const result = await getApiKeys({ limit: 200, cursor }, { signal: controller.signal, silent: true })
      items.push(...result.items)
      cursor = result.nextCursor ?? undefined
      if (cursor && seenCursors.has(cursor))
        throw new Error(t('plugins.frontendAuth.repeatedCursor'))
      if (cursor)
        seenCursors.add(cursor)
    } while (cursor)
    if (!controller.signal.aborted)
      clientKeys.value = items
  }
  catch (error: unknown) {
    if (!controller.signal.aborted) {
      keysFailed.value = true
      ZNotification.error({
        message: errorMessage(error, t('plugins.frontendAuth.failedToLoadClientKey')),
      })
    }
  }
  finally {
    if (keyLoadController === controller && !controller.signal.aborted) {
      loadingKeys.value = false
      keyLoadController = null
    }
  }
}

watch(validationError, error => emit('validityChange', !error), { immediate: true })
watch(() => props.active, (active, _, onCleanup) => {
  if (!active) {
    keyLoadController?.abort()
    keyLoadController = null
    return
  }
  void loadClientKeys()
  onCleanup(() => keyLoadController?.abort())
}, { immediate: true })

defineExpose({ validationError })
</script>

<template>
  <div class="grid gap-4">
    <div class="flex items-center gap-2 rounded-cp-lg bg-cp-fill-alter p-4">
      <ZSwitch
        :model-value="Boolean(binding)"
        :label="t('plugins.frontendAuth.enableClientAuthentication')"
        :disabled="disabled"
        @update:model-value="enableAuthentication"
      />
      <PluginHelpPopover :label="t('plugins.frontendAuth.helpTitle')">
        <p class="m-0">
          {{ t('plugins.frontendAuth.description') }}
        </p>
        <p class="m-0">
          {{ t('plugins.frontendAuth.loginNotice') }}
        </p>
        <p class="m-0 break-all font-mono text-cp-text-quaternary">
          {{ contribution.id }}
        </p>
      </PluginHelpPopover>
    </div>

    <template v-if="binding">
      <ZFormItem :label="t('plugins.frontendAuth.unmatchedProcessing')">
        <template #label-extra>
          <PluginHelpPopover :label="t('plugins.frontendAuth.unmatchedHandlingInstructions')">
            <p class="m-0">
              {{ t('plugins.frontendAuth.exclusiveHelp') }}
            </p>
            <p class="m-0">
              {{ t('plugins.frontendAuth.fallbackToApiKeyWhenNoMatch') }}
            </p>
            <p class="m-0">
              {{ t('plugins.frontendAuth.rejectionHelp') }}
            </p>
          </PluginHelpPopover>
        </template>
        <ZSegmented
          :model-value="binding.failurePolicy"
          :options="fallbackOptions"
          :aria-label="t('plugins.frontendAuth.unmatchedLabel')"
          :disabled="disabled"
          @update:model-value="setFailurePolicy"
        />
      </ZFormItem>

      <div class="grid gap-2">
        <div class="flex min-w-0 items-end justify-between gap-3">
          <div class="flex min-w-0 items-center gap-1.5">
            <p class="m-0 text-cp leading-none font-medium text-cp-text-secondary">
              {{ t('plugins.frontendAuth.identityMapping') }}
            </p>
            <PluginHelpPopover :label="t('plugins.frontendAuth.identityMappingDescription')">
              <p class="m-0">
                {{ t('plugins.frontendAuth.identityHelp') }}
              </p>
              <p class="m-0">
                {{ t('plugins.frontendAuth.keyHelp') }}
              </p>
            </PluginHelpPopover>
          </div>
          <div class="flex shrink-0 items-center gap-1">
            <ZIconButton v-if="keysFailed" size="small" :aria-label="t('plugins.frontendAuth.reloadClientKey')" :disabled="disabled" :loading="loadingKeys" @click="loadClientKeys()">
              <RefreshCw class="size-3.5" />
            </ZIconButton>
            <ZButton
              size="small"
              :disabled="disabled || binding.identityBindings.length >= 256"
              @click="addIdentity"
            >
              <template #icon>
                <Plus class="size-3.5" />
              </template>
              {{ t('plugins.frontendAuth.addMapping') }}
            </ZButton>
          </div>
        </div>

        <div
          v-for="(identity, index) in binding.identityBindings"
          :key="index"
          class="grid min-w-0 gap-3 rounded-cp-lg bg-cp-fill-alter p-3 md:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] md:items-end"
        >
          <ZFormItem :label="t('plugins.frontendAuth.identityMark')" required>
            <ZInput
              :model-value="identity.principal"
              maxlength="256"
              :placeholder="t('plugins.frontendAuth.identityPlaceholder')"
              :disabled="disabled"
              @update:model-value="updateIdentity(index, { principal: $event })"
            />
          </ZFormItem>
          <ZFormItem :label="t('plugins.bindings.clientKey')" required>
            <ZSelect
              :model-value="identity.clientKeyId"
              :options="keyOptions"
              :disabled="disabled || loadingKeys"
              :placeholder="loadingKeys ? t('common.status.loading') : keysFailed ? t('plugins.frontendAuth.failedToLoadTryAgain') : t('plugins.frontendAuth.selectClientApiKey')"
              :empty-text="t('plugins.frontendAuth.noClientKeyYet')"
              class="w-full min-w-0"
              @update:model-value="updateIdentity(index, { clientKeyId: $event })"
            />
          </ZFormItem>
          <ZIconButton
            size="default"
            :disabled="disabled"
            :aria-label="t('plugins.frontendAuth.removeIdentityMapping', { value: index + 1 })"
            class="justify-self-end"
            @click="removeIdentity(index)"
          >
            <Trash2 class="size-3.5" />
          </ZIconButton>
        </div>
      </div>
    </template>
  </div>
</template>
