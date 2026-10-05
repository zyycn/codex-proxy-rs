<script setup lang="ts">
import { BaseButton, BaseIconButton, BaseModal } from '@codex-proxy/ui'
import { Copy, Upload } from '@lucide/vue'

defineProps<{ createdKey: string }>()
const emit = defineEmits<{
  copy: [text: string]
  importCcs: []
  afterLeave: []
}>()
const open = defineModel<boolean>({ default: false })
</script>

<template>
  <BaseModal
    v-model="open"
    title="API Key 已创建"
    description="复制密钥，或直接导入 CCSwitch"
    tone="success"
    size="md"
    @after-leave="emit('afterLeave')"
  >
    <div class="flex flex-col gap-4">
      <div class="rounded-cp border border-cp-warning-border bg-cp-warning-container px-4 py-3">
        <p class="m-0 text-cp font-semibold text-cp-warning-on-container">
          该密钥具有网关访问权限，请仅发送给可信调用方
        </p>
      </div>
      <div>
        <p class="mb-2 text-cp font-medium text-cp-text-secondary">
          API Key
        </p>
        <div class="flex items-center gap-2">
          <code class="flex-1 rounded-cp bg-cp-fill-quaternary px-3 py-2.5 font-mono text-cp break-all text-cp-text">
            {{ createdKey }}
          </code>
          <BaseIconButton size="md" label="复制" @click="emit('copy', createdKey)">
            <Copy class="size-4" />
          </BaseIconButton>
        </div>
      </div>
    </div>

    <template #footer>
      <BaseButton variant="secondary" @click="emit('copy', createdKey)">
        <template #icon>
          <Copy class="size-4" />
        </template>
        复制密钥
      </BaseButton>
      <BaseButton variant="secondary" @click="emit('importCcs')">
        <template #icon>
          <Upload class="size-4" />
        </template>
        导入 CCSwitch
      </BaseButton>
      <BaseButton variant="primary" @click="open = false">
        我已保存
      </BaseButton>
    </template>
  </BaseModal>
</template>
