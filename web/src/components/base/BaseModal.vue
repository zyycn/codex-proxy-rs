<script setup lang="ts">
import { X } from '@lucide/vue'

import BaseButton from './BaseButton.vue'
import BaseIconButton from './BaseIconButton.vue'

defineProps<{
  title: string
  description?: string
}>()

const open = defineModel<boolean>({ default: false })
</script>

<template>
  <Teleport to="body">
    <div v-if="open" class="fixed inset-0 z-50 grid place-items-center p-6" role="presentation">
      <div class="absolute inset-0 bg-(--cp-overlay-scrim)" @click="open = false" />
      <section
        class="relative w-[min(560px,100%)] rounded-[22px] bg-(--cp-bg-surface) shadow-(--cp-shadow-popover)"
        role="dialog"
        aria-modal="true"
        :aria-label="title"
      >
        <header class="flex justify-between gap-4 p-6 pb-0">
          <div>
            <h2 class="m-0 text-lg font-[760] text-(--cp-text-primary)">{{ title }}</h2>
            <p v-if="description" class="mt-2 mb-0 text-[13px] font-semibold leading-tight text-(--cp-text-secondary)">
              {{ description }}
            </p>
          </div>
          <BaseIconButton label="关闭" @click="open = false">
            <X :size="16" />
          </BaseIconButton>
        </header>
        <div class="p-6">
          <slot />
        </div>
        <footer class="flex justify-end gap-3 px-6 pb-6">
          <slot name="footer">
            <BaseButton @click="open = false">取消</BaseButton>
            <BaseButton variant="primary" @click="open = false">确认</BaseButton>
          </slot>
        </footer>
      </section>
    </div>
  </Teleport>
</template>
