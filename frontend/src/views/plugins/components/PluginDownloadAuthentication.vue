<script setup lang="ts">
import type { CreatePluginSourceCredentialRequest, PluginSourceAuthentication, PluginSourceCredential } from '@/api'

import { ZFormItem, ZInput, ZSelect } from '@codex-proxy/ui'
import { computed, reactive, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { normalizePluginRepository } from '../utils/model'
import PluginHelpPopover from './PluginHelpPopover.vue'
import SourceCredentialPicker from './SourceCredentialPicker.vue'

const props = defineProps<{
  kind: 'url' | 'github'
  location: string
  credentials: PluginSourceCredential[]
  disabled: boolean
}>()

defineEmits<{
  deleteCredential: [credential: PluginSourceCredential]
}>()

const { t } = useI18n({ useScope: 'global' })

const selected = defineModel<string[]>({ required: true })
const pending = defineModel<boolean>('pending', { required: true })
const draft = defineModel<CreatePluginSourceCredentialRequest | null>('draft', { required: true })
const mode = shallowRef(selected.value.length ? 'saved' : 'public')
const form = reactive({
  kind: 'bearer',
  token: '',
  username: '',
  password: '',
  headerName: '',
  headerValue: '',
})
const authOptions = computed(() => ([
  {
    label: t('plugins.downloadAuth.accessToken'),
    value: 'bearer',
  },
  {
    label: t('plugins.downloadAuth.usernameAndPassword'),
    value: 'basic',
  },
  {
    label: t('plugins.downloadAuth.customRequestHeader'),
    value: 'header',
  },
]))
const modeOptions = computed(() => [
  {
    label: t('plugins.downloadAuth.noCertificationRequired'),
    value: 'public',
  },
  {
    label: props.kind === 'github'
      ? t('plugins.downloadAuth.fillInGithubToken')
      : t('plugins.downloadAuth.credentialsPlaceholder'),
    value: 'new',
  },
  ...(props.credentials.length
    ? [
        {
          label: t('plugins.downloadAuth.useSavedCredentials'),
          value: 'saved',
        },
      ]
    : []),
])
const scope = computed(() => {
  if (props.kind === 'github') {
    const repository = normalizePluginRepository(props.location)
    return /^[-\w.]+\/[-\w.]+$/.test(repository)
      ? { origin: 'https://api.github.com', pathPrefix: `/repos/${repository}`, name: `GitHub · ${repository}` }
      : null
  }
  try {
    const url = new URL(props.location.trim())
    const loopback = ['localhost', '127.0.0.1', '[::1]'].includes(url.hostname)
    if (!(url.protocol === 'https:' || (url.protocol === 'http:' && loopback)) || url.username || url.password || url.hash || /[%\\]/.test(url.pathname))
      return null
    return {
      origin: url.origin,
      pathPrefix: url.pathname,
      name: t('plugins.downloadAuth.download', { hostname: url.hostname }),
    }
  }
  catch {
    return null
  }
})
const authentication = computed<PluginSourceAuthentication | null>(() => {
  if (props.kind === 'github') {
    return form.token.trim()
      ? {
          kind: 'github',
          token: form.token.trim(),
        }
      : null
  }
  if (form.kind === 'basic') {
    return form.username.trim() && form.password
      ? {
          kind: 'basic',
          username: form.username.trim(),
          password: form.password,
        }
      : null
  }
  if (form.kind === 'header') {
    return form.headerName.trim() && form.headerValue
      ? {
          kind: 'header',
          name: form.headerName.trim(),
          value: form.headerValue,
        }
      : null
  }
  return form.token.trim()
    ? {
        kind: 'bearer',
        token: form.token.trim(),
      }
    : null
})

function clearSecrets() {
  Object.assign(form, {
    token: '',
    username: '',
    password: '',
    headerName: '',
    headerValue: '',
  })
}

function changeMode(value: string | undefined) {
  mode.value = value ?? 'public'
  selected.value = []
  clearSecrets()
}

watch(selected, (ids) => {
  // 弹窗统一提交保存成功后，切换为安全的凭据引用并清掉明文草稿。
  if (mode.value === 'new' && ids.length) {
    mode.value = 'saved'
    clearSecrets()
  }
})
watch(() => [props.kind, props.location], () => {
  // 切换目标必须重新选择凭据，不把当前目标的认证默认带到另一个来源。
  mode.value = 'public'
  selected.value = []
  clearSecrets()
})
watch(() => props.credentials, (credentials) => {
  selected.value = selected.value.filter(id => credentials.some(item => item.id === id))
  if (mode.value === 'saved' && !credentials.length)
    mode.value = 'public'
})
watch([mode, selected], () => {
  pending.value = mode.value === 'new' || (mode.value === 'saved' && !selected.value.length)
}, { immediate: true })
watch([mode, scope, authentication], () => {
  draft.value = mode.value === 'new' && scope.value && authentication.value
    ? {
        ...scope.value,
        name: scope.value.name.slice(0, 128),
        purposes: props.kind === 'github'
          ? ['metadata', 'artifact']
          : ['artifact'],
        authentication: authentication.value,
      }
    : null
}, { immediate: true })
</script>

<template>
  <div class="grid gap-4">
    <ZFormItem :label="t('plugins.downloadAuth.downloadCertification')">
      <template #label-extra>
        <PluginHelpPopover :label="t('plugins.downloadAuth.helpTitle')">
          <p class="m-0">
            {{ kind === 'github' ? t('plugins.downloadAuth.githubHelp') : t('plugins.downloadAuth.urlHelp') }}
          </p>
          <p class="m-0">
            {{ t('plugins.downloadAuth.storageHelp') }}
          </p>
        </PluginHelpPopover>
      </template>
      <ZSelect :model-value="mode" :options="modeOptions" :disabled="disabled" class="w-full" :aria-label="t('plugins.downloadAuth.downloadCertification')" @update:model-value="changeMode" />
    </ZFormItem>
    <template v-if="mode === 'new'">
      <ZFormItem v-if="kind === 'url'" :label="t('plugins.downloadAuth.authenticationMethod')">
        <ZSelect v-model="form.kind" :options="authOptions" :disabled="disabled" class="w-full" :aria-label="t('plugins.downloadAuth.authenticationMethod')" @update:model-value="clearSecrets" />
      </ZFormItem>
      <ZFormItem v-if="kind === 'github' || form.kind === 'bearer'" :label="kind === 'github' ? t('plugins.downloadAuth.githubToken') : t('plugins.downloadAuth.accessToken')" required>
        <ZInput v-model="form.token" type="password" autocomplete="new-password" :disabled="disabled" :aria-label="t('plugins.downloadAuth.downloadToken')" :placeholder="t('plugins.downloadAuth.enterToken')" />
      </ZFormItem>
      <div v-else-if="form.kind === 'basic'" class="grid gap-4 sm:grid-cols-2">
        <ZFormItem :label="t('plugins.downloadAuth.username')" required>
          <ZInput v-model="form.username" autocomplete="off" :disabled="disabled" :aria-label="t('plugins.downloadAuth.downloadUsername')" />
        </ZFormItem>
        <ZFormItem :label="t('plugins.downloadAuth.password')" required>
          <ZInput v-model="form.password" type="password" autocomplete="new-password" :disabled="disabled" :aria-label="t('plugins.downloadAuth.downloadPassword')" />
        </ZFormItem>
      </div>
      <div v-else class="grid gap-4 sm:grid-cols-2">
        <ZFormItem :label="t('plugins.downloadAuth.requestHeaderName')" required>
          <ZInput v-model="form.headerName" :disabled="disabled" :aria-label="t('plugins.downloadAuth.requestHeaderName')" placeholder="X-Download-Token" />
        </ZFormItem>
        <ZFormItem :label="t('plugins.downloadAuth.requestHeaderValue')" required>
          <ZInput v-model="form.headerValue" type="password" autocomplete="new-password" :disabled="disabled" :aria-label="t('plugins.downloadAuth.requestHeaderValue')" />
        </ZFormItem>
      </div>
    </template>
    <SourceCredentialPicker
      v-else-if="mode === 'saved'"
      v-model="selected"
      :credentials="credentials"
      :disabled="disabled"
      @delete="$emit('deleteCredential', $event)"
    />
  </div>
</template>
