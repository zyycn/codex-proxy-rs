<script setup lang="ts">
import type { PluginUpdateSelection } from '../composables/usePluginUpdateCheck'
import type { InstalledPlugin } from '../utils/catalog'
import { BaseButton, BaseModal, BaseTag } from '@codex-proxy/ui'
import { ArrowRight, Download, LoaderCircle, ShieldCheck } from '@lucide/vue'
import { computed } from 'vue'
import { isNewerPluginVersion } from '../utils/updates'
import PluginHelpPopover from './PluginHelpPopover.vue'
import PluginPermissionSummary from './PluginPermissionSummary.vue'

const props = defineProps<{ plugin: InstalledPlugin | null, result: PluginUpdateSelection | null, checking: boolean, upgrading: boolean }>()
defineEmits<{ install: [selection: PluginUpdateSelection], upgrade: [selection: PluginUpdateSelection] }>()
const open = defineModel<boolean>({ required: true })
const current = computed(() => props.plugin?.artifact.metadata)
const candidate = computed(() => props.result?.artifact?.metadata)
const newer = computed(() => Boolean(current.value && candidate.value && isNewerPluginVersion(current.value, candidate.value)))
const addedPermissions = computed(() => candidate.value?.permissionDescriptions.filter(permission => !current.value?.requestedPermissions.includes(permission.permission)) ?? [])
const canInstall = computed(() => props.result?.artifact || props.result?.release?.assets.some(asset => /\.(?:tar\.gz|tgz)$/i.test(asset.name)))
const status = computed(() => {
  if (!candidate.value)
    return '请选择插件包以确认版本与兼容性'
  if (current.value?.sha256 === candidate.value.sha256)
    return '当前已使用此版本'
  if (newer.value)
    return '发现新版本，插件包已校验'
  return current.value?.version === candidate.value.version ? '版本号相同，插件包内容不同' : '未发现更高版本'
})
</script>

<template>
  <BaseModal v-model="open" title="插件更新" :description="plugin?.artifact.metadata.displayName" size="md" :dismissible="!upgrading">
    <div v-if="checking" role="status" class="flex items-center gap-2 py-4 text-cp-sm text-cp-text-secondary">
      <LoaderCircle class="size-4 animate-spin motion-reduce:animate-none" />
      正在检查版本并校验插件包
    </div>
    <div v-else-if="result" class="grid gap-4 text-cp-sm">
      <div class="grid grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] items-center gap-4 rounded-cp bg-cp-fill-alter p-4" aria-label="版本对比">
        <div class="grid min-w-0 gap-2">
          <span class="text-cp-xs text-cp-text-secondary">当前版本</span>
          <strong class="break-all font-mono text-cp font-emphasis">{{ current?.version }}</strong>
        </div>
        <ArrowRight class="size-4 text-cp-text-quaternary" aria-hidden="true" />
        <div class="grid min-w-0 gap-2">
          <div class="flex flex-wrap items-center gap-2 text-cp-xs text-cp-text-secondary">
            <span>{{ candidate ? '目标版本' : '发布标签' }}</span>
            <BaseTag v-if="result.release?.prerelease" type="warning" size="sm">
              预发行版
            </BaseTag>
          </div>
          <strong class="break-all font-mono text-cp font-emphasis" :class="newer ? 'text-cp-primary-text' : 'text-cp-text'">{{ candidate?.version ?? result.release?.tag }}</strong>
        </div>
      </div>
      <div class="flex items-center gap-1.5 text-cp-xs text-cp-text-secondary">
        <span role="status">{{ status }}</span>
        <PluginHelpPopover label="更新检查说明">
          按插件包内版本判断升级，沿用当前设置，配置不兼容时保留当前版本并打开设置
        </PluginHelpPopover>
      </div>
      <div v-if="newer && addedPermissions.length" class="grid gap-3">
        <p class="m-0 font-emphasis">
          新增访问权限
        </p>
        <PluginPermissionSummary :permissions="addedPermissions" />
        <p class="m-0 text-cp-xs text-cp-text-secondary">
          升级即授权新增权限，请确认信任此插件
        </p>
      </div>
      <p v-else-if="newer" class="m-0 flex items-center gap-2 text-cp-xs text-cp-text-secondary">
        <ShieldCheck class="size-4 shrink-0" aria-hidden="true" />
        没有新增访问权限
      </p>
      <span v-if="!canInstall" class="text-cp-xs text-cp-text-secondary">此发布没有 tar.gz 或 tgz 插件包</span>
    </div>
    <template #footer>
      <BaseButton variant="secondary" :disabled="upgrading" @click="open = false">
        {{ checking ? '取消' : '关闭' }}
      </BaseButton>
      <BaseButton v-if="result?.instance && result.request && newer" variant="primary" :loading="upgrading" :disabled="upgrading" @click="$emit('upgrade', result)">
        <template #icon>
          <Download class="size-4" />
        </template>
        升级到 {{ candidate?.version }}
      </BaseButton>
      <BaseButton v-else-if="result && canInstall && (!candidate || (newer && !result.instance))" variant="primary" :disabled="upgrading" @click="$emit('install', result)">
        {{ candidate ? '继续安装' : '选择插件包' }}
      </BaseButton>
    </template>
  </BaseModal>
</template>
