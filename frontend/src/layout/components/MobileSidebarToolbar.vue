<script setup lang="ts">
import { Menu } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'

const emit = defineEmits<{
  open: []
}>()
</script>

<template>
  <div
    class="sticky top-0 z-40 -mx-4 -mt-4 mb-4 flex h-15 items-center bg-(--cp-bg-page)/96 px-4 backdrop-blur-sm min-[961px]:hidden"
  >
    <BaseButton
      icon-only
      size="md"
      variant="default"
      label="打开侧边栏"
      class="shadow-[0_12px_28px_-20px_var(--cp-shadow-sticky)]"
      @click="emit('open')"
    >
      <Menu class="size-5" />
    </BaseButton>
  </div>
</template>
