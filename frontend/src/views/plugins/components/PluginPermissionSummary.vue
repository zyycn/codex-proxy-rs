<script setup lang="ts">
import type { PluginPermissionDescription } from '@/api'
import { Database, Globe2, KeyRound, MessagesSquare, Network, ScanLine, Shield } from '@lucide/vue'

defineProps<{ permissions: PluginPermissionDescription[] }>()

const icons = {
  network: Globe2,
  models: MessagesSquare,
  accounts: KeyRound,
  data: Database,
  requests: ScanLine,
  public_endpoints: Network,
} as const

function permissionIcon(permission: string) {
  return icons[permission as keyof typeof icons] ?? Shield
}
</script>

<template>
  <dl v-if="permissions.length" class="m-0 grid gap-3">
    <div v-for="permission in permissions" :key="permission.permission" class="grid grid-cols-[1rem_minmax(0,1fr)] gap-x-2.5 gap-y-1">
      <component :is="permissionIcon(permission.permission)" class="mt-0.5 size-4 text-cp-text-secondary" aria-hidden="true" />
      <dt class="text-cp-sm font-emphasis text-cp-text">
        {{ permission.label }}
      </dt>
      <dd class="col-start-2 m-0 text-cp-xs leading-relaxed text-cp-text-secondary">
        {{ permission.description }}
      </dd>
    </div>
  </dl>
  <p v-else class="m-0 text-cp-sm text-cp-text-secondary">
    未声明额外的宿主访问权限
  </p>
</template>
