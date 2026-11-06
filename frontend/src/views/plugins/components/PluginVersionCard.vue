<script setup lang="ts">
import type { PluginArtifact, PluginInstance } from '@/api'

import { ZButton, ZIconButton, ZTag } from '@codex-proxy/ui'
import { ArrowDownToLine, ChevronDown, Play, Trash2 } from '@lucide/vue'
import { computed, shallowRef, useId } from 'vue'
import { useI18n } from 'vue-i18n'
import { PLUGIN_STATUS_LABELS } from '../constants'
import { configurationStatus, pluginStatusType } from '../utils/catalog'
import { shortDigest, sourceDetail, sourceLabel } from '../utils/model'
import PluginStatusNotice from './PluginStatusNotice.vue'

const props = defineProps<{
  artifact: PluginArtifact
  current?: PluginInstance
  configurations: PluginInstance[]
  busy: boolean
}>()

defineEmits<{
  switchVersion: [artifact: PluginArtifact]
  deleteVersion: [artifact: PluginArtifact]
  accept: [artifact: PluginArtifact]
}>()

const { t } = useI18n({ useScope: 'global' })

const isCurrent = computed(() => props.current?.artifactSha256 === props.artifact.metadata.sha256)
const uses = computed(() => props.configurations.filter(instance => instance.artifactSha256 === props.artifact.metadata.sha256))
const detailsOpen = shallowRef(false)
const detailsId = useId()
</script>

<template>
  <article class="min-w-0 rounded-z bg-z-fill-alter" :class="isCurrent ? 'grid min-h-36 content-between gap-3 p-4' : 'flex flex-wrap items-center gap-2 px-3 py-2 sm:gap-3'">
    <div v-if="isCurrent && current" class="flex flex-wrap items-center gap-2">
      <strong class="min-w-0 flex-1 wrap-break-word text-z-sm">{{ t('plugins.catalog.currentVersion') }}</strong>
      <ZTag>{{ artifact.metadata.version }}</ZTag>
      <PluginStatusNotice :instance="current" />
      <ZTag v-if="!['failed', 'incompatible'].includes(configurationStatus(current))" :type="pluginStatusType(configurationStatus(current))">
        {{ PLUGIN_STATUS_LABELS[configurationStatus(current)] }}
      </ZTag>
    </div>
    <div class="flex min-w-0 items-center justify-between gap-3" :class="isCurrent ? 'flex-wrap py-1' : 'flex-1'">
      <div class="grid min-w-0 gap-1 sm:flex sm:flex-wrap sm:items-center sm:gap-x-3">
        <strong v-if="!isCurrent" class="truncate text-z-sm">{{ artifact.metadata.version }}</strong>
        <span class="text-z-xs text-z-text-secondary">{{ sourceLabel(artifact.source) }}</span>
        <span class="truncate font-mono text-z-xs text-z-text-quaternary">{{ shortDigest(artifact.metadata.sha256) }}</span>
      </div>
      <ZButton
        v-if="isCurrent"
        type="primary"
        variant="link"
        size="small"
        class="h-auto! rounded-z! p-0! text-z-xs! leading-normal! font-normal! no-underline! [&>span]:gap-1"
        :aria-expanded="detailsOpen"
        :aria-controls="detailsId"
        @click="detailsOpen = !detailsOpen"
      >
        {{ t('plugins.versionDetails') }}
        <ChevronDown class="size-3.5 transition-transform motion-reduce:transition-none" :class="detailsOpen ? 'rotate-180' : undefined" aria-hidden="true" />
      </ZButton>
    </div>
    <ZTag v-if="!artifact.acceptedAt" type="warning" size="small">
      {{ t('plugins.toBeInstalled') }}
    </ZTag>
    <ZTag v-else-if="!isCurrent && uses.length" size="small">
      {{ t('plugins.historicalConfiguration') }}
    </ZTag>
    <dl v-show="detailsOpen" :id="detailsId" class="m-0 grid min-w-0 grid-cols-1 gap-x-8 gap-y-4 text-z-xs text-z-text-secondary sm:grid-cols-2" :class="!isCurrent ? 'order-last w-full pt-2' : undefined">
      <div class="grid min-w-0 content-start gap-1">
        <dt class="text-z-sm font-emphasis text-z-text">
          {{ sourceLabel(artifact.source) }}
        </dt>
        <dd class="m-0 break-all leading-relaxed">
          {{ sourceDetail(artifact.source) }}
        </dd>
      </div>
      <div v-if="uses.length" class="grid min-w-0 content-start gap-1">
        <dt class="text-z-sm font-emphasis text-z-text">
          {{ t('plugins.referenceConfiguration') }}
        </dt>
        <dd class="m-0 wrap-anywhere leading-relaxed">
          {{ uses.map(instance => instance.name).join('、') }}
        </dd>
      </div>
      <div class="grid min-w-0 content-start gap-1 sm:col-span-2">
        <dt class="text-z-sm font-emphasis text-z-text">
          SHA-256
        </dt>
        <dd class="m-0 break-all font-mono leading-relaxed">
          {{ artifact.metadata.sha256 }}
        </dd>
      </div>
    </dl>
    <div class="flex shrink-0 items-center gap-1" :class="isCurrent ? 'justify-self-end' : undefined">
      <ZIconButton v-if="!isCurrent" :aria-label="t('plugins.versions.detailsLabel', { version: artifact.metadata.version })" variant="solid" size="small" :aria-expanded="detailsOpen" :aria-controls="detailsId" @click="detailsOpen = !detailsOpen">
        <ChevronDown class="size-4 transition-transform motion-reduce:transition-none" :class="detailsOpen ? 'rotate-180' : undefined" />
      </ZIconButton>
      <ZIconButton v-if="!artifact.acceptedAt" :aria-label="t('plugins.install', { version: artifact.metadata.version })" size="small" type="primary" variant="solid" :disabled="busy" @click="$emit('accept', artifact)">
        <ArrowDownToLine class="size-4" />
      </ZIconButton>
      <ZIconButton v-else :aria-label="t('plugins.configuration.switchTo', { version: artifact.metadata.version })" size="small" variant="solid" :disabled="busy || !current || isCurrent" @click="$emit('switchVersion', artifact)">
        <Play class="size-4" />
      </ZIconButton>
      <ZIconButton v-if="artifact.acceptedAt" :aria-label="t('plugins.deleteVersion', { version: artifact.metadata.version })" size="small" variant="solid" class="group" :disabled="busy || uses.length > 0 || artifact.source.kind === 'builtin'" @click="$emit('deleteVersion', artifact)">
        <Trash2 class="size-4 text-z-error-text group-disabled:text-z-text-disabled" />
      </ZIconButton>
    </div>
  </article>
</template>
