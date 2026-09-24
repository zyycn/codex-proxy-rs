<script setup lang="ts">
import type { PluginArtifact, PluginInstance } from '@/api'
import { BaseIconButton, BaseTag } from '@codex-proxy/ui'
import { Play, Power, Settings2, Trash2 } from '@lucide/vue'
import { configurationStatus, PLUGIN_STATUS_LABELS, pluginStatusType } from '../utils/catalog'
import { artifactForInstance } from '../utils/model'

defineProps<{ instances: PluginInstance[], artifacts: PluginArtifact[], busy: boolean }>()
defineEmits<{
  edit: [instance: PluginInstance]
  enable: [instance: PluginInstance]
  disable: [instance: PluginInstance]
  delete: [instance: PluginInstance]
}>()
</script>

<template>
  <p v-if="instances.some(instance => instance.enabled)" role="status" class="m-0 text-cp-sm text-cp-warning-text">
    检测到多套启用配置，请在历史配置中停用不再使用的一套
  </p>
  <details v-if="instances.length" :open="instances.some(instance => instance.enabled)">
    <summary class="cursor-pointer text-cp-sm text-cp-text-secondary">
      历史配置 · {{ instances.length }}
    </summary>
    <p class="text-cp-xs text-cp-text-secondary">
      保留已有设置和数据，使用历史配置会替换当前启用配置
    </p>
    <div v-for="instance in instances" :key="instance.id" class="mt-2 flex flex-wrap items-center gap-2 rounded-cp bg-cp-fill-alter p-3">
      <span class="min-w-0 flex-1 wrap-anywhere text-cp-sm">{{ instance.name }}</span>
      <BaseTag size="sm">
        {{ artifactForInstance(instance, artifacts)?.metadata.version }}
      </BaseTag>
      <BaseTag size="sm" :type="pluginStatusType(configurationStatus(instance))">
        {{ PLUGIN_STATUS_LABELS[configurationStatus(instance)] }}
      </BaseTag>
      <div class="flex shrink-0 items-center gap-1">
        <BaseIconButton label="设置" variant="secondary" size="sm" :disabled="busy" @click="$emit('edit', instance)">
          <Settings2 class="size-4" />
        </BaseIconButton>
        <BaseIconButton v-if="instance.enabled" label="停用历史配置" variant="secondary" size="sm" :disabled="busy" @click="$emit('disable', instance)">
          <Power class="size-4" />
        </BaseIconButton>
        <BaseIconButton v-else label="启动" size="sm" variant="secondary" :disabled="busy" @click="$emit('enable', instance)">
          <Play class="size-4" />
        </BaseIconButton>
        <BaseIconButton v-if="!instance.enabled" label="删除历史配置" variant="secondary" size="sm" class="group" :disabled="busy" @click="$emit('delete', instance)">
          <Trash2 class="size-4 text-cp-error-text group-disabled:text-cp-text-disabled" />
        </BaseIconButton>
      </div>
    </div>
  </details>
</template>
