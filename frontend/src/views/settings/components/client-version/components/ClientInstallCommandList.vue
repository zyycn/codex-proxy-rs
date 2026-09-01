<script setup lang="ts">
import { Copy } from '@lucide/vue'

import BaseIconButton from '@/components/base/BaseIconButton.vue'
import { useCopyText } from '@/composables/useCopyText'

defineOptions({ name: 'ClientInstallCommandList' })

defineProps<{
  commands: Array<{
    label: string
    command: string
  }>
}>()

const copyText = useCopyText()

function copyCommand(command: string) {
  void copyText(command, { successText: '命令已复制' })
}
</script>

<template>
  <div class="grid gap-2.5">
    <article
      v-for="item in commands"
      :key="item.label"
      class="grid grid-cols-[minmax(0,1fr)_28px] items-start gap-2.5 rounded-cp bg-cp-bg-container px-3.5 py-3 shadow-cp-tertiary"
    >
      <div class="min-w-0">
        <p class="m-0 text-cp-xs font-heavy text-cp-text-quaternary">
          {{ item.label }}
        </p>
        <code class="mt-1.5 block overflow-x-auto whitespace-nowrap font-mono text-cp-sm leading-[1.5] font-semibold text-cp-text">{{ item.command }}</code>
      </div>
      <BaseIconButton size="sm" label="复制命令" @click="copyCommand(item.command)">
        <Copy class="size-3.5" />
      </BaseIconButton>
    </article>
  </div>
</template>
