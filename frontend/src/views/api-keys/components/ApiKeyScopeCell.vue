<script setup lang="ts">
import type { ApiKey } from '@/api'

import AccountGroupMarks from '@/components/AccountGroupMarks.vue'

defineProps<{
  apiKey: ApiKey
}>()
</script>

<template>
  <div class="grid gap-1.5">
    <div v-if="apiKey.routingScope === 'all'" class="flex items-center gap-2">
      <span class="inline-flex h-6 items-center rounded-lg bg-(--cp-warning-bg) px-2 text-[11px] font-bold text-(--cp-warning-text)">
        全部账号权限
      </span>
    </div>
    <template v-else>
      <AccountGroupMarks :groups="apiKey.groups" />
    </template>
  </div>
</template>
