<script setup lang="ts">
import type { ConfigurePluginInstanceRequest, PluginArtifact, PluginInstance, PluginVersionPlan } from '@/api'
import { BaseButton, BaseModal, BaseSegmented, toast } from '@codex-proxy/ui'
import { Blocks, Save, Settings2, ShieldCheck } from '@lucide/vue'
import { computed, nextTick, ref, shallowRef, useTemplateRef, watch } from 'vue'
import { cloneJsonValue, pluginContributionForCapability } from '../utils/model'
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
const authenticationContribution = computed(() => props.artifact && pluginContributionForCapability(props.artifact.metadata, 'frontend_authentication'))
const authenticationBinding = computed({
  get: () => bindings.value.find(binding => binding.contribution === authenticationContribution.value?.id) ?? null,
  set: (binding) => {
    const others = bindings.value.filter(value => value.contribution !== authenticationContribution.value?.id)
    bindings.value = binding ? [...others, cloneJsonValue(binding)] : others
  },
})
const hasRequestBindings = computed(() => Object.values(props.artifact?.metadata.contributes ?? {}).some(contribution => contribution.stages.some(stage => ['request', 'attempt', 'routing', 'scheduling', 'retry', 'observation'].includes(stage))))
const sections = computed(() => [
  { label: '插件参数', value: 'general', icon: Settings2 },
  ...(hasRequestBindings.value ? [{ label: '高级设置', value: 'requests', icon: Blocks }] : []),
  ...(authenticationContribution.value ? [{ label: '客户端认证', value: 'authentication', icon: ShieldCheck }] : []),
])
const primaryLabel = computed(() => versionChanged.value ? '应用并切换' : props.instance?.configurationRequired ? '保存并启用' : '保存设置')
const secretModes = [
  { label: '保留已有值', value: 'preserve' },
  { label: '替换全部值', value: 'replace' },
  { label: '清除全部值', value: 'clear' },
]

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
    toast.warning(configurationFields.value?.validationMessage || '请检查插件参数')
    await focusConfigurationInvalid()
    return
  }
  if (!bindingsValid.value) {
    section.value = 'requests'
    toast.warning('请设置请求范围，或勾选“应用于所有请求”')
    return
  }
  if (!authenticationValid.value) {
    section.value = 'authentication'
    toast.warning(authenticationEditor.value?.validationError || '请检查客户端认证身份映射')
    return
  }
  const input: ConfigurePluginInstanceRequest = {
    name: instance.name,
    artifactSha256: artifact.metadata.sha256,
    enabled: instance.enabled || (!versionChanged.value && instance.configurationRequired),
    configuration: cloneJsonValue(configuration.value),
    bindings: cloneJsonValue(bindings.value),
  }
  if (secretMode.value !== 'preserve')
    input.secrets = secretMode.value === 'clear' ? {} : cloneJsonValue(secretValues.value)
  emit('save', input)
}

watch(open, async (value) => {
  secretValues.value = {}
  if (!value)
    return
  configuration.value = cloneJsonValue(props.draft?.configuration ?? props.instance?.configuration ?? {})
  bindings.value = cloneJsonValue(props.draft?.bindings ?? props.instance?.bindings ?? [])
  secretMode.value = existingSecretFields.value.length ? 'preserve' : 'replace'
  configurationValid.value = true
  authenticationValid.value = true
  bindingsValid.value = true
  section.value = 'general'
  if (props.instance?.configurationRequired)
    await focusConfigurationInvalid()
}, { immediate: true })
</script>

<template>
  <BaseModal v-model="open" :title="versionChanged ? `切换至 ${artifact?.metadata.version}` : '插件设置'" :description="artifact ? `${artifact.metadata.displayName} · ${artifact.metadata.version}` : undefined" size="lg" :dismissible="!saving">
    <div v-if="artifact && instance" class="grid gap-5">
      <div v-if="error" role="alert" class="grid gap-1 text-cp-sm text-cp-warning-text">
        <span>{{ error }}</span>
        <span class="text-cp-xs text-cp-text-secondary">当前版本保持不变，修改后再应用</span>
      </div>
      <BaseSegmented v-if="sections.length > 1" v-model="section" :options="sections" label="插件设置分区" :disabled="saving" class="w-fit max-w-full" />
      <div v-show="section === 'general'" class="grid gap-5">
        <BaseSegmented
          v-if="existingSecretFields.length"
          v-model="secretMode"
          :options="secretModes"
          label="敏感配置保存方式"
          :disabled="saving"
          class="w-full sm:w-auto"
        />
        <PluginConfigurationFields
          ref="configurationFields"
          v-model:configuration="configuration"
          v-model:secrets="secretValues"
          :schema="artifact.metadata.configurationSchema"
          :secret-fields="artifact.metadata.secretFields"
          :existing-secret-fields="existingSecretFields"
          :secret-mode="secretMode"
          :disabled="saving"
          @validity-change="configurationValid = $event"
        />
      </div>
      <PluginBindingEditor
        v-if="open && hasRequestBindings"
        v-show="section === 'requests'"
        v-model="bindings"
        :metadata="artifact.metadata"
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
      <BaseButton variant="secondary" :disabled="saving" @click="open = false">
        取消
      </BaseButton>
      <BaseButton variant="primary" :loading="saving" :disabled="!instance || !artifact" @click="submit">
        <template #icon>
          <Save class="size-4" />
        </template>
        {{ primaryLabel }}
      </BaseButton>
    </template>
  </BaseModal>
</template>
