<script setup lang="ts">
import type { PluginUpdateSelection } from '../composables/usePluginUpdateCheck'

import type { PluginInstallMode, PluginInstallSelection } from '../utils/model'
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

import { ZButton, ZCheckbox, ZDialog, ZForm, ZFormItem, ZInput, ZSegmented, ZTag } from '@codex-proxy/ui'
import { CheckCircle2, Download, FileArchive, Link, PackageOpen } from '@lucide/vue'
import { useEventListener, useSessionStorage } from '@vueuse/core'

import { isEqual } from 'es-toolkit'
import { computed, nextTick, reactive, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { notification as ZNotification } from '@/utils/notification'
import { formatPluginFileSize, normalizePluginRepository, pluginInstallSelectionKey } from '../utils/model'
import { selectPluginReleaseAsset } from '../utils/updates'
import PluginAssetPicker from './PluginAssetPicker.vue'
import PluginDownloadAuthentication from './PluginDownloadAuthentication.vue'
import PluginHelpPopover from './PluginHelpPopover.vue'
import PluginSourceProxyField from './PluginSourceProxyField.vue'

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

const { t } = useI18n({ useScope: 'global' })

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
let activeDraftKey: string | null = null
const queriedGithubKey = shallowRef('')
const pendingAuthentication = shallowRef(false)
const credentialDraft = shallowRef<CreatePluginSourceCredentialRequest | null>(null)
const submitting = shallowRef(false)
const authenticationIncomplete = computed(() => pendingAuthentication.value && !credentialDraft.value)
const modeOptions = computed(() => ([
  {
    label: t('plugins.installation.uploadPackage'),
    value: 'upload',
    icon: FileArchive,
  },
  { label: 'URL', value: 'url', icon: Link },
  { label: 'GitHub', value: 'github', icon: Github },
]))
const busy = computed(() => submitting.value || props.saving || props.querying || props.verifying || props.savingCredential)
const connectionForm = computed(() => props.mode === 'url'
  ? urlForm
  : githubForm)

function changeMode(value: string) {
  if (busy.value || (value !== 'upload' && value !== 'url' && value !== 'github'))
    return
  emit('changeMode', value)
}
const releaseAssets = computed(() => (props.release?.assets ?? []).filter(asset => /\.(?:tar\.gz|tgz)$/i.test(asset.name)))
const digestPattern = /^[a-f0-9]{64}$/
const digestError = computed(() => {
  const digest = connectionForm.value.sha256.trim().toLowerCase()
  return digest && !digestPattern.test(digest)
    ? t('plugins.installation.enterDigitHexadecimalDigest')
    : ''
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
    location: {
      kind: 'github',
      repository: release.repository,
      tag: release.tag,
      asset: githubForm.asset,
      allow_prerelease: release.prerelease,
      sha256: sha256 || null,
    },
  }
})
const selection = computed(() => props.mode === 'upload'
  ? uploadFile.value
  : remoteRequest.value)
const requestKey = computed(() => selection.value
  ? pluginInstallSelectionKey(selection.value)
  : null)
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
const installationLabel = computed(() => previousArtifact.value
  ? t('plugins.installation.installNewVersion')
  : t('plugins.details.installation'))
const sourceDraft = computed<PluginUpdateSourceBinding | null>(() => {
  const previous = props.updateSource
  if (!previous)
    return null
  const source = props.mode === 'upload'
    ? { kind: 'upload' as const }
    : props.mode === 'url'
      ? {
          kind: 'url' as const,
          url: urlForm.url.trim(),
        }
      : {
          kind: 'github' as const,
          repository: githubRepository(),
        }
  return {
    pluginId: previous.pluginId,
    source,
    policy: isEqual(source, previous.source)
      ? previous.policy
      : { kind: 'manual' },
    outboundProxyId: props.mode === 'upload'
      ? null
      : connectionForm.value.outboundProxyId || null,
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
  activeDraftKey = props.acceptanceArtifact || props.updateSource
    ? null
    : 'install'
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
    githubForm.tag = source.policy.kind === 'pinned'
      ? source.policy.tag
      : ''
    githubForm.allowPrerelease = source.policy.kind === 'pinned' && source.policy.allow_prerelease
  }
  const draft = activeDraftKey ? drafts.value[activeDraftKey] : null
  if (draft) {
    Object.assign(urlForm, draft.url, {
      credentialIds: draft.url.credentialIds.filter(id => props.credentials.some(credential => credential.id === id)),
    })
    Object.assign(githubForm, draft.github, {
      credentialIds: draft.github.credentialIds.filter(id => props.credentials.some(credential => credential.id === id)),
    })
    emit('changeMode', draft.mode)
  }
  // 明确选择的检查结果优先于旧草稿，仍需重新校验包并确认信任来源。
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
      url: {
        ...urlForm,
        credentialIds: [...urlForm.credentialIds],
      },
      github: {
        ...github,
        credentialIds: [...github.credentialIds],
      },
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
    ZNotification.warning({
      message: t('plugins.installation.pluginPackageCannotExceedMib'),
    })
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
    ZNotification.warning({
      message: t('plugins.installation.verificationRequired'),
    })
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
    ZNotification.warning({
      message: t('plugins.installation.githubRepositoryRequired'),
    })
    return
  }
  if (githubForm.allowPrerelease && !tag) {
    ZNotification.warning({
      message: t('plugins.installation.prereleaseTagRequired'),
    })
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

watch(open, (isOpen) => {
  if (isOpen) {
    reset()
  }
  else {
    rememberDraft()
  }
})

function clearCredentials() {
  credentialDraft.value = null
}
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
    const previous = props.installedArtifacts.find(artifact => artifact.metadata.pluginId === props.updateSource?.pluginId)
    githubForm.asset = release
      ? selectPluginReleaseAsset(release, previous)?.name ?? ''
      : ''
    if (githubForm.asset) {
      await nextTick()
      await verifySelected()
    }
  },
)
</script>

<template>
  <ZDialog
    v-model="open"
    :title="updateSource ? t('plugins.installation.updatePlugin') : t('plugins.installation.installPlugin')"
    :description="acceptanceArtifact ? t('plugins.installation.installDescription') : updateSource ? t('plugins.installation.switchDescription') : t('plugins.installation.sourceHelp')"
    width="36rem"
    :show-close="!busy" :close-on-click-modal="!busy" :close-on-press-escape="!busy"
    @closed="clearCredentials"
  >
    <ZSegmented
      v-if="!acceptanceArtifact && !verified"
      :model-value="mode"
      :options="modeOptions"
      :disabled="busy"
      :aria-label="t('plugins.installation.installationMethod')"
      class="mb-5 w-full"
      @update:model-value="changeMode"
    />
    <ZForm v-if="!verified && mode === 'upload'">
      <ZFormItem :label="t('plugins.assets.packageLabel')" required>
        <!-- 原生文件输入放在样式化上传区内，保留键盘与读屏语义。 -->
        <!-- eslint-disable-next-line vue-a11y/label-has-for -->
        <label
          class="flex cursor-pointer items-center gap-3 rounded-cp bg-cp-fill-alter p-4 outline-none transition-colors hover:bg-cp-fill-tertiary focus-within:ring-2 focus-within:ring-cp-control-outline"
        >
          <FileArchive class="size-5 shrink-0 text-cp-text-secondary" aria-hidden="true" />
          <span class="grid min-w-0 flex-1 gap-1">
            <span class="truncate text-cp-sm font-emphasis text-cp-text">{{ uploadFile?.name ?? t('plugins.assets.selectPluginPackage') }}</span>
            <span class="text-cp-xs text-cp-text-secondary">{{ uploadFile ? formatPluginFileSize(uploadFile.size) : t('plugins.installation.tarGzTgzMaxMib') }}</span>
          </span>
          <span class="shrink-0 text-cp-xs text-cp-primary-text">{{ uploadFile ? t('plugins.installation.replace') : t('plugins.installation.browseFiles') }}</span>
          <input
            type="file"
            accept=".tar.gz,.tgz,application/gzip,application/x-gzip"
            class="sr-only"
            :disabled="busy"
            :aria-label="t('plugins.assets.selectPluginPackage')"
            @change="chooseFile"
          >
        </label>
      </ZFormItem>
    </ZForm>

    <ZForm v-else-if="!verified && mode === 'url'" class="grid gap-4">
      <ZFormItem :label="t('plugins.installation.downloadUrl')" required>
        <template #label-extra>
          <PluginHelpPopover :label="t('plugins.installation.downloadAddressDescription')">
            {{ t('plugins.installation.urlHelp') }}
          </PluginHelpPopover>
        </template>
        <ZInput v-model="urlForm.url" :disabled="busy" :aria-label="t('plugins.installation.downloadUrl')" placeholder="https://downloads.example.org/plugin.tar.gz" />
      </ZFormItem>
    </ZForm>

    <ZForm v-else-if="!verified" class="grid gap-4">
      <ZFormItem :label="t('plugins.installation.githubRepository')" required>
        <ZInput v-model="githubForm.repository" :disabled="busy" :aria-label="t('plugins.installation.githubRepository')" :placeholder="t('plugins.installation.repositoryLinkOrOwnerRepo')">
          <template #prefix>
            <Github class="size-4" />
          </template>
        </ZInput>
      </ZFormItem>
      <ZFormItem :label="t('plugins.installation.tagLabel')">
        <template #label-extra>
          <PluginHelpPopover :label="t('plugins.installation.tagHelpTitle')">
            {{ t('plugins.installation.tagHelp') }}
          </PluginHelpPopover>
        </template>
        <ZInput v-model="githubForm.tag" :disabled="busy" :aria-label="t('plugins.installation.tagLabel')" :placeholder="t('plugins.installation.tagPlaceholder')" />
      </ZFormItem>
      <ZCheckbox v-model="githubForm.allowPrerelease" :label="t('plugins.installation.allowPrereleaseLabel')" :disabled="busy" />
    </ZForm>

    <div v-if="!verified && mode !== 'upload'" class="mt-4 grid gap-4">
      <PluginDownloadAuthentication
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
        <ZFormItem :label="t('plugins.installation.verificationSummary')">
          <template #label-extra>
            <PluginHelpPopover :label="t('plugins.installation.digestHelpTitle')">
              {{ t('plugins.installation.digestHelp') }}
            </PluginHelpPopover>
          </template>
          <ZInput v-model="connectionForm.sha256" :disabled="busy" :aria-invalid="Boolean(digestError)" maxlength="64" :aria-label="t('plugins.installation.shaChecksum')" :placeholder="t('plugins.installation.optionalSha')" class="font-mono [&_input::placeholder]:font-normal" />
        </ZFormItem>
        <PluginSourceProxyField v-model="connectionForm.outboundProxyId" :disabled="busy" />
      </div>
    </div>

    <section v-if="!verified && mode === 'github' && release" class="mt-4 grid gap-3">
      <div class="flex min-w-0 flex-wrap items-center gap-2">
        <span class="text-cp-sm text-cp-text-secondary">{{ t('plugins.installation.versionFound') }}</span>
        <strong class="min-w-0 wrap-break-word text-cp-sm font-emphasis text-cp-text">{{ release.tag }}</strong>
        <ZTag v-if="release.prerelease" type="warning">
          {{ t('plugins.installation.preRelease') }}
        </ZTag>
      </div>
      <ZFormItem v-if="releaseAssets.length > 1" :label="t('plugins.assets.packageLabel')" required>
        <PluginAssetPicker v-model="githubForm.asset" :assets="releaseAssets" :disabled="busy" />
      </ZFormItem>
      <p v-else class="m-0 break-all text-cp-sm text-cp-text-secondary">
        {{ githubForm.asset || t('plugins.installation.thereIsNoTarGzOrTgz') }}
      </p>
    </section>

    <section v-if="verified" :aria-label="t('plugins.installation.pendingTitle')" class="rounded-cp bg-cp-fill-alter p-4">
      <div class="flex items-start gap-3">
        <div class="flex size-10 shrink-0 items-center justify-center rounded-cp bg-cp-bg-container text-cp-text-secondary">
          <PackageOpen class="size-5" aria-hidden="true" />
        </div>
        <div class="min-w-0 flex-1">
          <div class="flex flex-wrap items-center gap-x-2 gap-y-1">
            <h3 class="m-0 min-w-0 wrap-break-word text-cp-sm font-emphasis text-cp-text">
              {{ verified.metadata.displayName }}
            </h3>
            <ZTag>{{ verified.metadata.version }}</ZTag>
            <span role="status" class="ml-auto inline-flex shrink-0 items-center gap-1 text-cp-xs text-cp-success-text">
              <CheckCircle2 class="size-3.5" aria-hidden="true" />
              {{ t('plugins.installation.parsingCompleted') }}
            </span>
          </div>
          <p class="mt-1 mb-0 break-all font-mono text-cp-xs text-cp-text-secondary" :title="`SHA-256 ${verified.metadata.sha256}`">
            {{ verified.metadata.pluginId }}
          </p>
        </div>
      </div>
      <dl class="mt-3 mb-0 grid grid-cols-[auto_minmax(0,1fr)] items-center gap-x-4 gap-y-2 text-cp-xs leading-relaxed">
        <dt class="text-cp-text-secondary">
          {{ t('plugins.installation.publisher') }}
        </dt>
        <dd class="m-0 min-w-0 wrap-break-word">
          {{ verified.metadata.publisher }}
        </dd>
        <dt class="text-cp-text-secondary">
          {{ t('plugins.installation.applicablePlatforms') }}
        </dt>
        <dd class="m-0 flex min-w-0 flex-wrap gap-1.5">
          <ZTag v-for="platform in verified.metadata.platforms" :key="platform" size="small">
            {{ platform }}
          </ZTag>
        </dd>
      </dl>
      <p class="mt-4 mb-0 text-cp-xs leading-relaxed text-cp-text-secondary">
        {{ t('plugins.installation.trustNotice') }}
      </p>
      <div v-if="previousArtifact" class="mt-2 flex items-center gap-1.5 text-cp-xs text-cp-text-secondary">
        <span>{{ t('plugins.installation.activateAfterInstall') }}</span>
        <PluginHelpPopover :label="t('plugins.installation.installNewVersionInstructions')">
          {{ t('plugins.installation.configurationHelp') }}
        </PluginHelpPopover>
      </div>
    </section>

    <template #footer>
      <ZButton v-if="verified && !acceptanceArtifact" variant="text" :disabled="busy" class="mr-auto" @click="$emit('resetVerification')">
        {{ t('common.actions.back') }}
      </ZButton>
      <ZButton v-if="!verified && mode === 'github' && release" :disabled="busy || authenticationIncomplete" @click="queryRelease">
        {{ t('plugins.installation.queryAgain') }}
      </ZButton>
      <ZButton :disabled="busy" @click="open = false">
        {{ t('common.actions.cancel') }}
      </ZButton>
      <ZButton v-if="mode === 'github' && !release" type="primary" :loading="submitting || querying" :disabled="busy || authenticationIncomplete || !githubForm.repository.trim()" @click="queryRelease">
        {{ querying ? t('plugins.installation.queryingVersion') : sourceChanged ? t('plugins.installation.saveSourceAndContinue') : t('common.actions.next') }}
      </ZButton>
      <ZButton
        v-else-if="!verified"
        type="primary"
        :loading="submitting || verifying"
        :disabled="!selection || busy || authenticationIncomplete"
        @click="verifySelected"
      >
        {{ verifying ? t('plugins.installation.parsingPluginPackage') : sourceChanged ? t('plugins.installation.saveSourceAndContinue') : t('common.actions.next') }}
      </ZButton>
      <ZButton v-if="verified" type="primary" :loading="saving" :disabled="busy || pendingAuthentication" @click="installSelected">
        <template #icon>
          <Download class="size-4" />
        </template>
        {{ installationLabel }}
      </ZButton>
    </template>
  </ZDialog>
</template>
