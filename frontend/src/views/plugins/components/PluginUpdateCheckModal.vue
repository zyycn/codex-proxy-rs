<script setup lang="ts">
import type { PluginUpdateSelection } from '../composables/usePluginUpdateCheck'

import type { InstalledPlugin } from '../utils/catalog'
import { ZButton, ZDialog, ZTag } from '@codex-proxy/ui'
import { ArrowRight, Download, LoaderCircle } from '@lucide/vue'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { isNewerPluginVersion } from '../utils/updates'
import PluginHelpPopover from './PluginHelpPopover.vue'

const props = defineProps<{ plugin: InstalledPlugin | null, result: PluginUpdateSelection | null, checking: boolean, upgrading: boolean }>()

defineEmits<{ install: [selection: PluginUpdateSelection], upgrade: [selection: PluginUpdateSelection] }>()

const { t } = useI18n({ useScope: 'global' })

const open = defineModel<boolean>({ required: true })
const current = computed(() => props.plugin?.artifact.metadata)
const candidate = computed(() => props.result?.artifact?.metadata)
const newer = computed(() => Boolean(current.value && candidate.value && isNewerPluginVersion(current.value, candidate.value)))
const canInstall = computed(() => props.result?.artifact || props.result?.release?.assets.some(asset => /\.(?:tar\.gz|tgz)$/i.test(asset.name)))
const status = computed(() => {
  if (!candidate.value)
    return t('plugins.updates.packageRequired')
  if (current.value?.sha256 === candidate.value.sha256)
    return t('plugins.updates.currentlyUsingThisVersion')
  if (newer.value)
    return t('plugins.updates.verifiedUpdate')
  return current.value?.version === candidate.value.version
    ? t('plugins.updates.sameVersionDifferentContent')
    : t('plugins.updates.noHigherVersionFound')
})
</script>

<template>
  <ZDialog v-model="open" :title="t('plugins.updates.pluginUpdate')" :description="plugin?.artifact.metadata.displayName" width="36rem" :show-close="!upgrading" :close-on-click-modal="!upgrading" :close-on-press-escape="!upgrading">
    <div v-if="checking" role="status" class="flex items-center gap-2 py-4 text-cp-sm text-cp-text-secondary">
      <LoaderCircle class="size-4 animate-spin motion-reduce:animate-none" />
      {{ t('plugins.updates.checkingDescription') }}
    </div>
    <div v-else-if="result" class="grid gap-4 text-cp-sm">
      <div class="grid grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] items-center gap-4 rounded-cp bg-cp-fill-alter p-4" :aria-label="t('plugins.updates.versionComparison')">
        <div class="grid min-w-0 gap-2">
          <span class="text-cp-xs text-cp-text-secondary">{{ t('plugins.catalog.currentVersion') }}</span>
          <strong class="break-all font-mono text-cp font-emphasis">{{ current?.version }}</strong>
        </div>
        <ArrowRight class="size-4 text-cp-text-quaternary" aria-hidden="true" />
        <div class="grid min-w-0 gap-2">
          <div class="flex flex-wrap items-center gap-2 text-cp-xs text-cp-text-secondary">
            <span>{{ candidate ? t('plugins.rollback.targetVersion') : t('plugins.updates.postTag') }}</span>
            <ZTag v-if="result.release?.prerelease" type="warning" size="small">
              {{ t('plugins.installation.preRelease') }}
            </ZTag>
          </div>
          <strong class="break-all font-mono text-cp font-emphasis" :class="newer ? 'text-cp-primary-text' : 'text-cp-text'">{{ candidate?.version ?? result.release?.tag }}</strong>
        </div>
      </div>
      <div class="flex items-center gap-1.5 text-cp-xs text-cp-text-secondary">
        <span role="status">{{ status }}</span>
        <PluginHelpPopover :label="t('plugins.updates.updateInspectionInstructions')">
          {{ t('plugins.updates.configurationHelp') }}
        </PluginHelpPopover>
      </div>
      <p v-if="newer" class="m-0 text-cp-xs leading-relaxed text-cp-text-secondary">
        {{ t('plugins.updates.trustNotice') }}
      </p>
      <span v-if="!canInstall" class="text-cp-xs text-cp-text-secondary">{{ t('plugins.updates.thereIsNoTarGzOrTgzPlugin') }}</span>
    </div>
    <template #footer>
      <ZButton :disabled="upgrading" @click="open = false">
        {{ checking ? t('common.actions.cancel') : t('common.actions.close') }}
      </ZButton>
      <ZButton v-if="result?.instance && result.request && newer" type="primary" :loading="upgrading" :disabled="upgrading" @click="$emit('upgrade', result)">
        <template #icon>
          <Download class="size-4" />
        </template>
        {{ t('plugins.updates.upgradeTo', { version: candidate?.version }) }}
      </ZButton>
      <ZButton v-else-if="result && canInstall && (!candidate || (newer && !result.instance))" type="primary" :disabled="upgrading" @click="$emit('install', result)">
        {{ candidate ? t('plugins.updates.continueInstallation') : t('plugins.assets.selectPluginPackage') }}
      </ZButton>
    </template>
  </ZDialog>
</template>
