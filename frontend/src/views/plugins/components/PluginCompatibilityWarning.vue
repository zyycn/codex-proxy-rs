<script setup lang="ts">
import type { PluginInstance } from '@/api'
import PluginHelpPopover from './PluginHelpPopover.vue'

defineProps<{ instance: PluginInstance }>()
</script>

<template>
  <PluginHelpPopover v-if="instance.compatibilityWarning" :label="`${instance.name}不兼容`" warning>
    <p class="m-0 font-emphasis text-cp-text">
      {{ instance.enabled ? '不兼容，无法启动' : '已停用，无法启动' }}
    </p>
    <p class="m-0">
      {{ instance.compatibilityWarning }}
    </p>
    <p class="m-0">
      配置与数据保留，安装兼容版本后可重新启用
    </p>
  </PluginHelpPopover>
</template>
