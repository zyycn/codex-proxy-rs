<script setup lang="ts">
import type { ConfigurePluginInstanceRequest, PluginArtifact, PluginInstance, PluginVersionPlan } from '@/api'

import { ZButton, ZDialog, ZSegmented } from '@codex-proxy/ui'
import { Blocks, Save, Settings2, ShieldCheck } from '@lucide/vue'
import { cloneDeep } from 'es-toolkit'
import { computed, nextTick, ref, shallowRef, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { notification as ZNotification } from '@/utils/notification'
import { pluginRequestBindingEntries } from '../utils/model'
import PluginBindingEditor from './PluginBindingEditor.vue'
import PluginConfigurationFields from './PluginConfigurationFields.vue'
import PluginFrontendAuthenticationEditor from './PluginFrontendAuthenticationEditor.vue'

const props = defineProps<{
  instance: PluginInstance | null
  artifact: PluginArtifact | null
  draft: PluginVersionPlan | null
  error: string
  saving: boolean
}>()

const emit = defineEmits<{ save: [value: ConfigurePluginInstanceRequest] }>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ required: true })
const section = shallowRef('general')
const configurationValid = shallowRef(true)
const authenticationValid = shallowRef(true)
const bindingsValid = shallowRef(true)
const secretMode = shallowRef<'preserve' | 'replace' | 'clear'>('preserve')
const secretValues = ref<Record<string, string>>({})
const configuration = ref<Record<string, unknown>>({})
const bindings = ref<ConfigurePluginInstanceRequest['bindings']>([])
const configurationFields = useTemplateRef<{ focusInvalid: () => Promise<unknown>, validationMessage: string }>('configurationFields')
const authenticationEditor = useTemplateRef<{ validationError: string }>('authenticationEditor')
const existingSecretFields = computed(() => props.draft?.secretFields ?? props.instance?.secretFields ?? [])
const versionChanged = computed(() => Boolean(props.instance && props.artifact && props.instance.artifactSha256 !== props.artifact.metadata.sha256))
const authenticationContribution = computed(() => props.artifact && props.artifact.metadata.contributes.frontend_authentication)
const authenticationBinding = computed({
  get: () => bindings.value.find(binding => binding.contribution === authenticationContribution.value?.id) ?? null,
  set: (binding) => {
    const others = bindings.value.filter(value => value.contribution !== authenticationContribution.value?.id)
    bindings.value = binding
      ? [
          ...others,
          cloneDeep(binding),
        ]
      : others
  },
})
const hasRequestBindings = computed(() => props.artifact && pluginRequestBindingEntries(props.artifact.metadata).length > 0)
const sections = computed(() => [
  {
    label: t('plugins.configuration.pluginParameters'),
    value: 'general',
    icon: Settings2,
  },
  ...(hasRequestBindings.value
    ? [
        {
          label: t('plugins.configuration.advancedSettings'),
          value: 'requests',
          icon: Blocks,
        },
      ]
    : []),
  ...(authenticationContribution.value
    ? [
        {
          label: t('plugins.configuration.clientAuthentication'),
          value: 'authentication',
          icon: ShieldCheck,
        },
      ]
    : []),
])
const primaryLabel = computed(() => versionChanged.value
  ? t('plugins.configuration.applyAndSwitch')
  : props.instance?.configurationRequired
    ? t('plugins.configuration.saveAndEnable')
    : t('plugins.configuration.saveSettings'))

async function focusConfigurationInvalid() {
  section.value = 'general'
  await nextTick()
  await configurationFields.value?.focusInvalid()
}

async function submit() {
  const instance = props.instance
  const artifact = props.artifact
  if (!instance || !artifact)
    return
  if (!configurationValid.value) {
    ZNotification.warning({ message: configurationFields.value?.validationMessage || t('plugins.configuration.checkPluginParameters') })
    await focusConfigurationInvalid()
    return
  }
  if (!bindingsValid.value) {
    section.value = 'requests'
    ZNotification.warning({
      message: t('plugins.configuration.setRequestScopeOrCheckApplyTo'),
    })
    return
  }
  if (!authenticationValid.value) {
    section.value = 'authentication'
    ZNotification.warning({ message: authenticationEditor.value?.validationError || t('plugins.configuration.invalidIdentityMappings') })
    return
  }
  const input: ConfigurePluginInstanceRequest = {
    name: instance.name,
    artifactSha256: artifact.metadata.sha256,
    enabled: instance.enabled || (!versionChanged.value && instance.configurationRequired),
    configuration: cloneDeep(configuration.value),
    bindings: cloneDeep(bindings.value),
  }
  if (secretMode.value !== 'preserve') {
    input.secrets = secretMode.value === 'clear'
      ? {}
      : cloneDeep(secretValues.value)
  }
  emit('save', input)
}

watch(open, async (value) => {
  if (!value)
    return
  secretValues.value = {}
  configuration.value = cloneDeep(props.draft?.configuration ?? props.instance?.configuration ?? {})
  bindings.value = cloneDeep(props.draft?.bindings ?? props.instance?.bindings ?? [])
  secretMode.value = existingSecretFields.value.length ? 'preserve' : 'replace'
  configurationValid.value = true
  authenticationValid.value = true
  bindingsValid.value = true
  section.value = 'general'
  if (props.instance?.configurationRequired)
    await focusConfigurationInvalid()
}, { immediate: true })

function clearSecrets() {
  secretValues.value = {}
}
</script>

<template>
  <ZDialog v-model="open" :title="versionChanged ? t('plugins.configuration.switchTo', { version: artifact?.metadata.version }) : t('plugins.configuration.pluginSettings')" :description="artifact ? `${artifact.metadata.displayName} · ${artifact.metadata.version}` : undefined" width="48rem" :show-close="!saving" :close-on-click-modal="!saving" :close-on-press-escape="!saving" @closed="clearSecrets">
    <div v-if="artifact && instance" class="grid gap-5">
      <div v-if="error" role="alert" class="grid gap-1 text-cp-sm text-cp-warning-text">
        <span>{{ error }}</span>
        <span class="text-cp-xs text-cp-text-secondary">{{ t('plugins.configuration.unchangedVersion') }}</span>
      </div>
      <ZSegmented v-if="sections.length > 1" v-model="section" :options="sections" :aria-label="t('plugins.configuration.pluginSettingsPartition')" :disabled="saving" class="w-fit max-w-full" />
      <div v-show="section === 'general'" class="grid gap-5">
        <PluginConfigurationFields
          ref="configurationFields"
          v-model:configuration="configuration"
          v-model:secrets="secretValues"
          v-model:secret-mode="secretMode"
          :schema="artifact.metadata.configurationSchema"
          :secret-fields="artifact.metadata.secretFields"
          :existing-secret-fields="existingSecretFields"
          :disabled="saving"
          @validity-change="configurationValid = $event"
        />
      </div>
      <PluginBindingEditor
        v-if="hasRequestBindings"
        v-show="section === 'requests'"
        :key="`${instance.id}:${artifact.metadata.sha256}`"
        v-model="bindings"
        :metadata="artifact.metadata"
        :active="open"
        :disabled="saving"
        @validity-change="bindingsValid = $event"
      />
      <PluginFrontendAuthenticationEditor
        v-if="authenticationContribution"
        v-show="section === 'authentication'"
        ref="authenticationEditor"
        v-model="authenticationBinding"
        :contribution="authenticationContribution"
        :active="open"
        :disabled="saving"
        @validity-change="authenticationValid = $event"
      />
    </div>
    <template #footer>
      <ZButton :disabled="saving" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton type="primary" :loading="saving" :disabled="!instance || !artifact" @click="submit">
        <template #icon>
          <Save class="size-4" />
        </template>
        {{ primaryLabel }}
      </ZButton>
    </template>
  </ZDialog>
</template>
