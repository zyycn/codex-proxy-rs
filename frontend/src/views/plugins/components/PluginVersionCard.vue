<script setup lang="ts">
import type { PluginArtifact, PluginInstance } from '@/api'
import { BaseIconButton, BaseTag } from '@codex-proxy/ui'
import { ArrowDownToLine, ChevronDown, CircleAlert, Play, Trash2 } from '@lucide/vue'
import { computed, shallowRef, useId } from 'vue'
import { configurationStatus, PLUGIN_STATUS_LABELS, pluginStatusType } from '../utils/catalog'
import { shortDigest, sourceDetail, sourceLabel } from '../utils/model'

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
const isCurrent = computed(() => props.current?.artifactSha256 === props.artifact.metadata.sha256)
const uses = computed(() => props.configurations.filter(instance => instance.artifactSha256 === props.artifact.metadata.sha256))
const detailsOpen = shallowRef(false)
const detailsId = useId()
</script>

<template>
  <article class="min-w-0 rounded-cp bg-cp-fill-alter" :class="isCurrent ? 'grid min-h-36 content-between gap-3 p-4' : 'flex flex-wrap items-center gap-2 px-3 py-2 sm:gap-3'">
    <div v-if="isCurrent && current" class="flex flex-wrap items-center gap-2">
      <strong class="min-w-0 flex-1 wrap-break-word text-cp-sm">当前版本</strong>
      <BaseTag>{{ artifact.metadata.version }}</BaseTag>
      <BaseTag :type="pluginStatusType(configurationStatus(current))">
        <CircleAlert v-if="configurationStatus(current) === 'failed'" class="mr-1 size-3.5" />
        {{ PLUGIN_STATUS_LABELS[configurationStatus(current)] }}
      </BaseTag>
    </div>
    <div class="flex min-w-0 items-center justify-between gap-3" :class="isCurrent ? 'flex-wrap py-1' : 'flex-1'">
      <div class="grid min-w-0 gap-1 sm:flex sm:flex-wrap sm:items-center sm:gap-x-3">
        <strong v-if="!isCurrent" class="truncate text-cp-sm">{{ artifact.metadata.version }}</strong>
        <span class="text-cp-xs text-cp-text-secondary">{{ sourceLabel(artifact.source) }}</span>
        <span class="truncate font-mono text-cp-xs text-cp-text-quaternary">{{ shortDigest(artifact.metadata.sha256) }}</span>
      </div>
      <button
        v-if="isCurrent"
        type="button"
        class="inline-flex shrink-0 cursor-pointer items-center gap-1 rounded-cp border-0 bg-transparent p-0 text-cp-xs text-cp-primary-text outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline"
        :aria-expanded="detailsOpen"
        :aria-controls="detailsId"
        @click="detailsOpen = !detailsOpen"
      >
        版本详情
        <ChevronDown class="size-3.5 transition-transform motion-reduce:transition-none" :class="detailsOpen ? 'rotate-180' : undefined" aria-hidden="true" />
      </button>
    </div>
    <BaseTag v-if="!artifact.acceptedAt" type="warning" size="sm">
      待安装
    </BaseTag>
    <BaseTag v-else-if="!isCurrent && uses.length" size="sm">
      历史配置
    </BaseTag>
    <dl :id="detailsId" v-show="detailsOpen" class="m-0 grid min-w-0 grid-cols-1 gap-x-8 gap-y-4 text-cp-xs text-cp-text-secondary sm:grid-cols-2" :class="!isCurrent ? 'order-last w-full pt-2' : undefined">
      <div class="grid min-w-0 content-start gap-1">
        <dt class="text-cp-sm font-emphasis text-cp-text">
          {{ sourceLabel(artifact.source) }}
        </dt>
        <dd class="m-0 break-all leading-relaxed">
          {{ sourceDetail(artifact.source) }}
        </dd>
      </div>
      <div v-if="uses.length" class="grid min-w-0 content-start gap-1">
        <dt class="text-cp-sm font-emphasis text-cp-text">
          引用配置
        </dt>
        <dd class="m-0 wrap-anywhere leading-relaxed">
          {{ uses.map(instance => instance.name).join('、') }}
        </dd>
      </div>
      <div class="grid min-w-0 content-start gap-1 sm:col-span-2">
        <dt class="text-cp-sm font-emphasis text-cp-text">
          SHA-256
        </dt>
        <dd class="m-0 break-all font-mono leading-relaxed">
          {{ artifact.metadata.sha256 }}
        </dd>
      </div>
    </dl>
    <div class="flex shrink-0 items-center gap-1" :class="isCurrent ? 'justify-self-end' : undefined">
      <BaseIconButton v-if="!isCurrent" :label="`${artifact.metadata.version} 版本详情`" variant="secondary" size="sm" :aria-expanded="detailsOpen" :aria-controls="detailsId" @click="detailsOpen = !detailsOpen">
        <ChevronDown class="size-4 transition-transform motion-reduce:transition-none" :class="detailsOpen ? 'rotate-180' : undefined" />
      </BaseIconButton>
      <BaseIconButton v-if="!artifact.acceptedAt" :label="`安装 ${artifact.metadata.version}`" size="sm" variant="primary" :disabled="busy" @click="$emit('accept', artifact)">
        <ArrowDownToLine class="size-4" />
      </BaseIconButton>
      <BaseIconButton v-else :label="`切换至 ${artifact.metadata.version}`" size="sm" variant="secondary" :disabled="busy || !current || isCurrent" @click="$emit('switchVersion', artifact)">
        <Play class="size-4" />
      </BaseIconButton>
      <BaseIconButton v-if="artifact.acceptedAt" :label="`删除版本 ${artifact.metadata.version}`" size="sm" variant="secondary" class="group" :disabled="busy || uses.length > 0 || artifact.source.kind === 'builtin'" @click="$emit('deleteVersion', artifact)">
        <Trash2 class="size-4 text-cp-error-text group-disabled:text-cp-text-disabled" />
      </BaseIconButton>
    </div>
  </article>
</template>
