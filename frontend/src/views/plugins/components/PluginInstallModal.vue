<script setup lang="ts">
import type { PluginUpdateSelection } from '../composables/usePluginUpdateCheck'
import type { PluginInstallSelection } from '../utils/model'
import type {
  CreatePluginSourceCredentialRequest,
  PluginArtifact,
  PluginRelease,
  PluginSourceCredential,
  PluginUpdateSourceBinding,
  QueryPluginReleaseRequest,
  VerifiedPluginArtifact,
  VerifyRemotePluginRequest,
} from '@/api'

import { Github } from '@boxicons/vue'
import { BaseButton, BaseCheckbox, BaseForm, BaseFormItem, BaseInput, BaseModal, BaseSegmented, BaseSelect, BaseTag, toast } from '@codex-proxy/ui'

import { CheckCircle2, Download, FileArchive, Link, PackageOpen } from '@lucide/vue'
import { useEventListener, useSessionStorage } from '@vueuse/core'
import { isEqual } from 'es-toolkit'
import { computed, nextTick, reactive, shallowRef, watch } from 'vue'
import { formatDateTime } from '@/utils/date'
import { normalizePluginRepository, pluginInstallSelectionKey } from '../utils/model'
import PluginDownloadAuthentication from './PluginDownloadAuthentication.vue'
import PluginHelpPopover from './PluginHelpPopover.vue'
import PluginPermissionSummary from './PluginPermissionSummary.vue'
import PluginSourceProxyField from './PluginSourceProxyField.vue'

export type PluginInstallMode = 'upload' | 'url' | 'github'

const props = defineProps<{
  mode: PluginInstallMode
  installedArtifacts: PluginArtifact[]
  acceptanceArtifact: PluginArtifact | null
  updateSource: PluginUpdateSourceBinding | null
  updateSelection: PluginUpdateSelection | null
  saveSource: (source: PluginUpdateSourceBinding) => Promise<boolean>
  credentials: PluginSourceCredential[]
  savingCredential: boolean
  saveCredential: (request: CreatePluginSourceCredentialRequest) => Promise<PluginSourceCredential | null>
  release: PluginRelease | null
  saving: boolean
  querying: boolean
  verifying: boolean
  verified: { requestKey: string | File, artifact: VerifiedPluginArtifact } | null
}>()

const emit = defineEmits<{
  install: [request: PluginInstallSelection]
  accept: [artifact: PluginArtifact]
  query: [request: QueryPluginReleaseRequest]
  verify: [request: PluginInstallSelection]
  resetVerification: []
  resetQuery: []
  changeMode: [mode: PluginInstallMode]
  deleteCredential: [credential: PluginSourceCredential]
}>()

const open = defineModel<boolean>({ required: true })
const uploadFile = shallowRef<File | null>(null)
const urlForm = reactive({
  url: '',
  sha256: '',
  credentialIds: [] as string[],
  outboundProxyId: '',
})
const githubForm = reactive({
  repository: '',
  tag: '',
  allowPrerelease: false,
  credentialIds: [] as string[],
  outboundProxyId: '',
  asset: '',
  sha256: '',
})
interface InstallDraft {
  mode: PluginInstallMode
  url: typeof urlForm
  github: Omit<typeof githubForm, 'asset'>
}
const drafts = useSessionStorage<Record<string, InstallDraft>>('cp-plugin-install-drafts', {}, { flush: 'sync' })
const draftKey = computed(() => props.updateSource ? `plugin:${props.updateSource.pluginId}` : 'install')
let activeDraftKey: string | null = null
const queriedGithubKey = shallowRef('')
const pendingAuthentication = shallowRef(false)
const credentialDraft = shallowRef<CreatePluginSourceCredentialRequest | null>(null)
const submitting = shallowRef(false)
const authenticationIncomplete = computed(() => pendingAuthentication.value && !credentialDraft.value)
const modeOptions = [
  { label: '上传包', value: 'upload', icon: FileArchive },
  { label: 'URL', value: 'url', icon: Link },
  { label: 'GitHub', value: 'github', icon: Github },
]
const busy = computed(() => submitting.value || props.saving || props.querying || props.verifying || props.savingCredential)
const connectionForm = computed(() => props.mode === 'url' ? urlForm : githubForm)

function changeMode(value: string) {
  if (busy.value || (value !== 'upload' && value !== 'url' && value !== 'github'))
    return
  emit('changeMode', value)
}
const assetOptions = computed(() => (props.release?.assets ?? []).filter(asset => /\.(?:tar\.gz|tgz)$/i.test(asset.name)).map(asset => ({
  label: `${asset.name} · ${formatFileSize(asset.size)}`,
  value: asset.name,
  description: asset.sha256 ? `SHA-256 ${asset.sha256}` : undefined,
})))
const digestPattern = /^[a-f0-9]{64}$/
const digestError = computed(() => {
  const digest = connectionForm.value.sha256.trim().toLowerCase()
  return digest && !digestPattern.test(digest) ? '请输入 64 位十六进制摘要' : ''
})
const remoteRequest = computed<VerifyRemotePluginRequest | null>(() => {
  if (digestError.value)
    return null
  if (props.mode === 'url') {
    const url = urlForm.url.trim()
    const sha256 = urlForm.sha256.trim().toLowerCase()
    return url
      ? {
          expectedPluginId: props.updateSource?.pluginId ?? null,
          credentialIds: [...urlForm.credentialIds],
          outboundProxyId: urlForm.outboundProxyId || null,
          location: { kind: 'url', url, sha256: sha256 || null },
        }
      : null
  }
  const release = props.release
  const sha256 = githubForm.sha256.trim().toLowerCase()
  if (props.mode !== 'github' || !release || !githubForm.asset)
    return null
  return {
    expectedPluginId: props.updateSource?.pluginId ?? null,
    credentialIds: [...githubForm.credentialIds],
    outboundProxyId: githubForm.outboundProxyId || null,
    location: { kind: 'github', repository: release.repository, tag: release.tag, asset: githubForm.asset, allow_prerelease: release.prerelease, sha256: sha256 || null },
  }
})
const selection = computed(() => props.mode === 'upload' ? uploadFile.value : remoteRequest.value)
const requestKey = computed(() => selection.value ? pluginInstallSelectionKey(selection.value) : null)
const verified = computed<VerifiedPluginArtifact | null>(() => {
  if (props.acceptanceArtifact) {
    return {
      metadata: props.acceptanceArtifact.metadata,
      source: props.acceptanceArtifact.source,
    }
  }
  return !pendingAuthentication.value && props.verified?.requestKey === requestKey.value
    ? props.verified.artifact
    : null
})
const previousArtifact = computed(() => props.installedArtifacts
  .filter(artifact => artifact.acceptedAt && artifact.metadata.pluginId === verified.value?.metadata.pluginId)
  .sort((left, right) => right.acceptedAt!.localeCompare(left.acceptedAt!))[0])
const addedPermissions = computed(() => verified.value?.metadata.permissionDescriptions.filter(permission =>
  !previousArtifact.value?.metadata.requestedPermissions.includes(permission.permission),
) ?? [])
const installationLabel = computed(() => previousArtifact.value ? '安装新版本' : '安装')
const sourceDraft = computed<PluginUpdateSourceBinding | null>(() => {
  const previous = props.updateSource
  if (!previous)
    return null
  const source = props.mode === 'upload'
    ? { kind: 'upload' as const }
    : props.mode === 'url'
      ? { kind: 'url' as const, url: urlForm.url.trim() }
      : { kind: 'github' as const, repository: githubRepository() }
  return {
    pluginId: previous.pluginId,
    source,
    policy: isEqual(source, previous.source) ? previous.policy : { kind: 'manual' },
    outboundProxyId: props.mode === 'upload' ? null : connectionForm.value.outboundProxyId || null,
  }
})
const sourceChanged = computed(() => sourceDraft.value !== null && !isEqual(sourceDraft.value, { ...props.updateSource, outboundProxyId: props.updateSource?.outboundProxyId ?? null }))

async function prepareSource() {
  return !sourceChanged.value || (sourceDraft.value !== null && await props.saveSource(sourceDraft.value))
}

watch(requestKey, () => emit('resetVerification'))

async function prepareAuthentication() {
  if (!pendingAuthentication.value)
    return true
  if (!credentialDraft.value)
    return false
  const credential = await props.saveCredential(credentialDraft.value)
  if (!credential)
    return false
  connectionForm.value.credentialIds = [credential.id]
  // 等认证字段切换为已保存引用、旧校验失效后，再发起下载，避免 watcher 取消新请求。
  await nextTick()
  return true
}

async function verifySelected() {
  if (busy.value || !selection.value || authenticationIncomplete.value)
    return
  submitting.value = true
  try {
    if (await prepareSource() && await prepareAuthentication() && selection.value)
      emit('verify', selection.value)
  }
  finally {
    submitting.value = false
  }
}

function reset() {
  activeDraftKey = props.acceptanceArtifact ? null : draftKey.value
  uploadFile.value = null
  Object.assign(urlForm, {
    url: '',
    sha256: '',
    credentialIds: [],
    outboundProxyId: '',
  })
  Object.assign(githubForm, {
    repository: '',
    tag: '',
    allowPrerelease: false,
    credentialIds: [],
    outboundProxyId: '',
    asset: '',
    sha256: '',
  })
  queriedGithubKey.value = ''
  pendingAuthentication.value = false
  credentialDraft.value = null
  const history = props.installedArtifacts
    .filter(artifact => artifact.metadata.pluginId === props.updateSource?.pluginId)
    .sort((left, right) => right.installedAt.localeCompare(left.installedAt))
  const previousUrl = history.find(artifact => artifact.source.kind === 'url')?.source
  const previousGithub = history.find(artifact => artifact.source.kind === 'github')?.source
  if (previousUrl?.kind === 'url') {
    urlForm.url = previousUrl.url
    urlForm.credentialIds = previousUrl.credential_ids.filter(id => props.credentials.some(credential => credential.id === id))
    urlForm.outboundProxyId = previousUrl.outbound_proxy?.id ?? ''
  }
  if (previousGithub?.kind === 'github') {
    githubForm.repository = previousGithub.repository
    githubForm.tag = previousGithub.tag
    githubForm.credentialIds = previousGithub.credential_ids.filter(id => props.credentials.some(credential => credential.id === id))
    githubForm.outboundProxyId = previousGithub.outbound_proxy?.id ?? ''
  }
  const source = props.updateSource
  if (source?.source.kind === 'url') {
    if (urlForm.url !== source.source.url)
      urlForm.credentialIds = []
    urlForm.url = source.source.url
    urlForm.outboundProxyId = source.outboundProxyId ?? ''
  }
  if (source?.source.kind === 'github') {
    if (githubForm.repository !== source.source.repository) {
      githubForm.credentialIds = []
      githubForm.tag = ''
    }
    githubForm.repository = source.source.repository
    githubForm.outboundProxyId = source.outboundProxyId ?? ''
    if (source.policy.kind !== 'manual')
      githubForm.tag = source.policy.kind === 'pinned' ? source.policy.tag : ''
    githubForm.allowPrerelease = source.policy.kind === 'pinned' && source.policy.allow_prerelease
  }
  const draft = drafts.value[draftKey.value]
  if (draft && !props.acceptanceArtifact) {
    Object.assign(urlForm, draft.url, { credentialIds: draft.url.credentialIds.filter(id => props.credentials.some(credential => credential.id === id)) })
    Object.assign(githubForm, draft.github, { credentialIds: draft.github.credentialIds.filter(id => props.credentials.some(credential => credential.id === id)) })
    emit('changeMode', draft.mode)
  }
  // 明确选择的检查结果优先于旧草稿，仍需重新校验包并确认安装权限。
  const selection = props.updateSelection
  if (selection?.binding.source.kind === 'github' && selection.release) {
    Object.assign(githubForm, {
      repository: selection.release.repository,
      tag: selection.release.tag,
      allowPrerelease: selection.release.prerelease,
      credentialIds: [...selection.credentialIds],
      outboundProxyId: selection.binding.outboundProxyId ?? '',
      sha256: '',
    })
    emit('changeMode', 'github')
  }
  else if (selection?.binding.source.kind === 'url' && selection.artifact) {
    Object.assign(urlForm, {
      url: selection.binding.source.url,
      credentialIds: [...selection.credentialIds],
      outboundProxyId: selection.binding.outboundProxyId ?? '',
      sha256: selection.artifact.metadata.sha256,
    })
    emit('changeMode', 'url')
  }
  emit('resetQuery')
}

function rememberDraft() {
  if (!activeDraftKey)
    return
  // 只保存安装参数和凭据引用，不缓存文件、令牌、密码或认证明文草稿。
  const { asset: _asset, ...github } = githubForm
  drafts.value = {
    ...drafts.value,
    [activeDraftKey]: {
      mode: props.mode,
      url: { ...urlForm, credentialIds: [...urlForm.credentialIds] },
      github: { ...github, credentialIds: [...github.credentialIds] },
    },
  }
}
useEventListener('pagehide', () => {
  if (open.value)
    rememberDraft()
})

function chooseFile(event: Event) {
  const file = (event.target as HTMLInputElement).files?.[0] ?? null
  if (file && file.size > 32 * 1024 * 1024) {
    toast.warning('插件包不能超过 32 MiB')
    uploadFile.value = null
    ;(event.target as HTMLInputElement).value = ''
    return
  }
  uploadFile.value = file
}

function installSelected() {
  if (props.acceptanceArtifact) {
    emit('accept', props.acceptanceArtifact)
    return
  }
  if (!selection.value || !verified.value) {
    toast.warning('请先校验当前选择的插件包')
    return
  }
  emit('install', selection.value)
}

function githubRepository() {
  return normalizePluginRepository(githubForm.repository)
}

async function queryRelease() {
  if (busy.value || authenticationIncomplete.value)
    return
  const repository = githubRepository()
  const tag = githubForm.tag.trim()
  if (!/^[-\w.]+\/[-\w.]+$/.test(repository)) {
    toast.warning('请输入 GitHub 仓库链接或 owner/repo')
    return
  }
  if (githubForm.allowPrerelease && !tag) {
    toast.warning('查询预发行版需要明确填写 tag')
    return
  }
  submitting.value = true
  try {
    if (!await prepareSource() || !await prepareAuthentication())
      return
    queriedGithubKey.value = githubQueryKey()
    emit('query', {
      query: {
        repository,
        tag: tag || null,
        allowPrerelease: githubForm.allowPrerelease,
      },
      credentialIds: [...githubForm.credentialIds],
      outboundProxyId: githubForm.outboundProxyId || null,
    })
  }
  finally {
    submitting.value = false
  }
}

function githubQueryKey() {
  return JSON.stringify({
    repository: githubRepository().toLowerCase(),
    tag: githubForm.tag.trim(),
    allowPrerelease: githubForm.allowPrerelease,
    credentialIds: [...githubForm.credentialIds].sort(),
    outboundProxyId: githubForm.outboundProxyId || null,
  })
}

function formatFileSize(bytes: number) {
  if (bytes < 1024)
    return `${bytes} B`
  if (bytes < 1024 * 1024)
    return `${(bytes / 1024).toFixed(1)} KiB`
  return `${(bytes / 1024 / 1024).toFixed(1)} MiB`
}

watch(open, (isOpen) => {
  if (isOpen) {
    reset()
  }
  else {
    rememberDraft()
    credentialDraft.value = null
  }
})
watch(() => props.mode, () => {
  pendingAuthentication.value = false
  credentialDraft.value = null
})
watch([pendingAuthentication, credentialDraft], () => {
  emit('resetVerification')
  if (props.mode === 'github') {
    queriedGithubKey.value = ''
    emit('resetQuery')
  }
})
watch(
  () => githubQueryKey(),
  (key) => {
    if (!queriedGithubKey.value || key === queriedGithubKey.value)
      return
    queriedGithubKey.value = ''
    emit('resetQuery')
  },
)
watch(
  () => props.release,
  async (release) => {
    // 唯一归档可直接解析，多个归档交给用户选择，平台兼容性仍由包检查器确认。
    githubForm.asset = release && assetOptions.value.length === 1 ? assetOptions.value[0]!.value : ''
    if (githubForm.asset) {
      await nextTick()
      await verifySelected()
    }
  },
)
</script>

<template>
  <BaseModal
    v-model="open"
    :title="updateSource ? '更新插件' : '安装插件'"
    :description="acceptanceArtifact ? '确认访问权限后安装' : updateSource ? '检查设置兼容性后切换版本' : '支持本地插件包、URL 与 GitHub'"
    size="md"
    :dismissible="!busy"
  >
    <BaseSegmented
      v-if="!acceptanceArtifact && !verified"
      :model-value="mode"
      :options="modeOptions"
      :disabled="busy"
      label="安装方式"
      class="mb-5 w-full"
      @update:model-value="changeMode"
    />
    <BaseForm v-if="!verified && mode === 'upload'">
      <BaseFormItem label="插件包" required>
        <!-- 原生文件输入放在样式化上传区内，保留键盘与读屏语义。 -->
        <!-- eslint-disable-next-line vue-a11y/label-has-for -->
        <label
          class="flex cursor-pointer items-center gap-3 rounded-cp bg-cp-fill-alter p-4 outline-none transition-colors hover:bg-cp-fill-tertiary focus-within:ring-2 focus-within:ring-cp-control-outline"
        >
          <FileArchive class="size-5 shrink-0 text-cp-text-secondary" aria-hidden="true" />
          <span class="grid min-w-0 flex-1 gap-1">
            <span class="truncate text-cp-sm font-emphasis text-cp-text">{{ uploadFile?.name ?? '选择插件包' }}</span>
            <span class="text-cp-xs text-cp-text-secondary">{{ uploadFile ? formatFileSize(uploadFile.size) : 'tar.gz / tgz，最大 32 MiB' }}</span>
          </span>
          <span class="shrink-0 text-cp-xs text-cp-primary-text">{{ uploadFile ? '更换' : '浏览文件' }}</span>
          <input
            type="file"
            accept=".tar.gz,.tgz,application/gzip,application/x-gzip"
            class="sr-only"
            :disabled="busy"
            aria-label="选择插件包"
            @change="chooseFile"
          >
        </label>
      </BaseFormItem>
    </BaseForm>

    <BaseForm v-else-if="!verified && mode === 'url'" class="grid gap-4">
      <BaseFormItem label="下载 URL" required>
        <template #label-extra>
          <PluginHelpPopover label="下载地址说明">
            仅支持 HTTPS，本机回环地址可使用 HTTP
          </PluginHelpPopover>
        </template>
        <BaseInput v-model="urlForm.url" :disabled="busy" aria-label="下载 URL" placeholder="https://downloads.example.org/plugin.tar.gz" />
      </BaseFormItem>
    </BaseForm>

    <BaseForm v-else-if="!verified" class="grid gap-4">
      <BaseFormItem label="GitHub 仓库" required>
        <BaseInput v-model="githubForm.repository" :disabled="busy" aria-label="GitHub 仓库" placeholder="仓库链接或 owner/repo">
          <template #prefix>
            <Github class="size-4" />
          </template>
        </BaseInput>
      </BaseFormItem>
      <BaseFormItem label="版本标签">
        <template #label-extra>
          <PluginHelpPopover label="版本标签说明">
            留空查询最新稳定版，查询预发行版时必须指定标签
          </PluginHelpPopover>
        </template>
        <BaseInput v-model="githubForm.tag" :disabled="busy" aria-label="版本标签" placeholder="留空查最新稳定版" />
      </BaseFormItem>
      <BaseCheckbox v-model="githubForm.allowPrerelease" label="允许查询预发行版" show-label :disabled="busy" />
    </BaseForm>

    <div v-if="!verified && mode !== 'upload'" class="mt-4 grid gap-4">
      <PluginDownloadAuthentication
        v-if="open"
        :key="mode"
        v-model="connectionForm.credentialIds"
        v-model:pending="pendingAuthentication"
        v-model:draft="credentialDraft"
        :kind="mode"
        :location="mode === 'github' ? githubForm.repository : urlForm.url"
        :credentials="credentials"
        :disabled="busy"
        @delete-credential="$emit('deleteCredential', $event)"
      />
      <div class="grid items-end gap-4 sm:grid-cols-2">
        <BaseFormItem label="校验摘要">
          <template #label-extra>
            <PluginHelpPopover label="校验摘要说明">
              可选，填写发布者提供的 64 位 SHA-256 十六进制摘要，用于核对下载内容
            </PluginHelpPopover>
          </template>
          <BaseInput v-model="connectionForm.sha256" :disabled="busy" :aria-invalid="Boolean(digestError)" maxlength="64" aria-label="SHA-256 校验摘要" placeholder="可选，SHA-256" class="font-mono [&_input::placeholder]:font-normal" />
        </BaseFormItem>
        <PluginSourceProxyField v-model="connectionForm.outboundProxyId" :disabled="busy" />
      </div>
    </div>

    <section v-if="!verified && mode === 'github' && release" class="mt-4 grid gap-3">
      <div class="flex min-w-0 flex-wrap items-center gap-2">
        <strong class="min-w-0 wrap-break-word text-cp text-cp-text">{{ release.name || release.tag }}</strong>
        <BaseTag type="primary">
          {{ release.tag }}
        </BaseTag>
        <BaseTag v-if="release.prerelease" type="warning">
          预发行版
        </BaseTag>
      </div>
      <BaseFormItem v-if="assetOptions.length > 1" label="插件包" required>
        <BaseSelect v-model="githubForm.asset" :options="assetOptions" :disabled="busy" placeholder="选择插件包" class="w-full" />
      </BaseFormItem>
      <p v-else class="m-0 break-all text-cp-sm text-cp-text-secondary">
        {{ githubForm.asset || '此版本没有 tar.gz 或 tgz 插件包' }}
      </p>
    </section>

    <section v-if="verified" aria-label="待安装插件" class="rounded-cp bg-cp-fill-alter p-4">
      <div class="flex items-start gap-3">
        <div class="flex size-10 shrink-0 items-center justify-center rounded-cp bg-cp-bg-container text-cp-text-secondary">
          <PackageOpen class="size-5" aria-hidden="true" />
        </div>
        <div class="min-w-0 flex-1">
          <div class="flex flex-wrap items-center gap-x-2 gap-y-1">
            <h3 class="m-0 min-w-0 break-words text-cp-sm font-emphasis text-cp-text">
              {{ verified.metadata.displayName }}
            </h3>
            <BaseTag>{{ verified.metadata.version }}</BaseTag>
            <span role="status" class="ml-auto inline-flex shrink-0 items-center gap-1 text-cp-xs text-cp-success-text">
              <CheckCircle2 class="size-3.5" aria-hidden="true" />
              解析完成
            </span>
          </div>
          <p class="mt-1 mb-0 break-all font-mono text-cp-xs text-cp-text-secondary" :title="`SHA-256 ${verified.metadata.sha256}`">
            {{ verified.metadata.pluginId }}
          </p>
        </div>
      </div>
      <dl class="mt-3 mb-0 grid grid-cols-[auto_minmax(0,1fr)] items-center gap-x-4 gap-y-2 text-cp-xs leading-relaxed">
        <dt class="text-cp-text-secondary">
          发布者
        </dt>
        <dd class="m-0 min-w-0 break-words">
          {{ verified.metadata.publisher }}
        </dd>
        <dt class="text-cp-text-secondary">
          适用平台
        </dt>
        <dd class="m-0 flex min-w-0 flex-wrap gap-1.5">
          <BaseTag v-for="platform in verified.metadata.platforms" :key="platform" size="sm">
            {{ platform }}
          </BaseTag>
        </dd>
      </dl>
      <div class="mt-4 grid gap-3">
        <p v-if="previousArtifact" class="m-0 text-cp-sm font-emphasis">
          {{ addedPermissions.length ? '新增访问权限' : '没有新增访问权限' }}
        </p>
        <PluginPermissionSummary v-if="!previousArtifact || addedPermissions.length" :permissions="addedPermissions" />
        <details v-if="previousArtifact && verified.metadata.permissionDescriptions.length" class="text-cp-xs text-cp-text-secondary">
          <summary class="cursor-pointer py-1 outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline">
            全部访问权限
          </summary>
          <PluginPermissionSummary class="mt-3" :permissions="verified.metadata.permissionDescriptions" />
        </details>
      </div>
      <p class="mt-4 mb-0 text-cp-xs leading-relaxed text-cp-text-secondary">
        安装即授权，插件以网关身份运行，请仅安装可信来源
      </p>
      <div v-if="previousArtifact" class="mt-2 flex items-center gap-1.5 text-cp-xs text-cp-text-secondary">
        <span>安装后确认切换到新版本</span>
        <PluginHelpPopover label="安装新版本说明">
          沿用设置并补充新版本默认值，需要调整时会打开设置，已使用版本保留恢复设置
        </PluginHelpPopover>
      </div>
    </section>

    <template #footer>
      <BaseButton v-if="verified && !acceptanceArtifact" variant="ghost" :disabled="busy" class="mr-auto" @click="$emit('resetVerification')">
        返回
      </BaseButton>
      <BaseButton v-if="!verified && mode === 'github' && release" variant="secondary" :disabled="busy || authenticationIncomplete" :title="`缓存至 ${formatDateTime(release.expiresAt)}`" @click="queryRelease">
        重新查询
      </BaseButton>
      <BaseButton variant="secondary" :disabled="busy" @click="open = false">
        取消
      </BaseButton>
      <BaseButton v-if="mode === 'github' && !release" variant="primary" :loading="submitting || querying" :disabled="busy || authenticationIncomplete || !githubForm.repository.trim()" @click="queryRelease">
        {{ querying ? '正在查询版本' : sourceChanged ? '保存来源并继续' : '下一步' }}
      </BaseButton>
      <BaseButton
        v-else-if="!verified"
        variant="primary"
        :loading="submitting || verifying"
        :disabled="!selection || busy || authenticationIncomplete"
        @click="verifySelected"
      >
        {{ verifying ? '正在解析插件包' : sourceChanged ? '保存来源并继续' : '下一步' }}
      </BaseButton>
      <BaseButton v-if="verified" variant="primary" :loading="saving" :disabled="busy || pendingAuthentication" @click="installSelected">
        <template #icon>
          <Download class="size-4" />
        </template>
        {{ installationLabel }}
      </BaseButton>
    </template>
  </BaseModal>
</template>
