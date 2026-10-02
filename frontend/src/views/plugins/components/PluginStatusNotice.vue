<script setup lang="ts">
import type { PluginInstance } from '@/api'
import { computed } from 'vue'
import PluginHelpPopover from './PluginHelpPopover.vue'

const props = defineProps<{ instance: PluginInstance }>()
const failure = computed(() => props.instance.loadError ?? props.instance.runtime.failure?.message)
const title = computed(() => failure.value ? (props.instance.loadError ? '无法启动' : '运行失败') : props.instance.compatibilityWarning ? '兼容性提醒' : '接口待升级，当前仍兼容')
</script>

<template>
  <PluginHelpPopover
    v-if="failure || instance.compatibilityWarning || instance.apiDeprecations.length"
    :label="`${instance.name}：${title}`"
    :tone="failure ? 'error' : 'warning'"
  >
    <p class="m-0 font-emphasis text-cp-text">
      {{ title }}
    </p>
    <p v-if="failure" class="m-0 text-cp-error-text">
      {{ failure }}
    </p>
    <p v-if="instance.runtime.failure" class="m-0 font-mono">
      {{ instance.runtime.failure.code }}
    </p>
    <template v-if="instance.compatibilityWarning">
      <p class="m-0 text-cp-warning">
        {{ instance.compatibilityWarning }}
      </p>
      <p class="m-0">
        可以尝试启动，运行结果仍需验证
      </p>
    </template>
    <div v-for="entry in instance.apiDeprecations" :key="`${entry.capability}:${entry.version}`" class="grid gap-1">
      <p class="m-0 font-mono text-cp-text">
        {{ entry.capability }} v{{ entry.version }} → v{{ entry.replacementVersion }}
      </p>
      <p class="m-0 text-cp-warning">
        {{ entry.remainingReleases > 0 ? `旧接口还将兼容至少 ${entry.remainingReleases} 个正式版本` : '兼容窗口已满，后续正式版本将移除旧接口转换' }}
      </p>
      <p class="m-0">
        {{ entry.migration }}
      </p>
    </div>
    <p v-if="instance.loadError" class="m-0">
      配置与数据保留，请安装可加载的插件版本
    </p>
    <p v-else-if="failure" class="m-0">
      启用配置保留，可在插件详情中重新启动
    </p>
    <p v-else-if="instance.apiDeprecations.length" class="m-0">
      请更新插件，或联系作者升级接口
    </p>
  </PluginHelpPopover>
</template>
