<script setup lang="ts">
import type { AccountRow } from '../constants'
import { KeyRound, MoreHorizontal, Pencil, RefreshCw, RotateCcw, Trash2, Wifi } from '@lucide/vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BasePopover from '@/components/base/BasePopover.vue'

defineProps<{
  account: AccountRow
  deleting: boolean
  recovering: boolean
  refreshing: boolean
  testing: boolean
}>()

const emit = defineEmits<{
  edit: [account: AccountRow]
  delete: [account: AccountRow]
  recover: [accountId: string]
  test: [account: AccountRow]
  refresh: [accountId: string]
  reauthorize: [account: AccountRow]
}>()
</script>

<template>
  <div class="relative flex items-center justify-start gap-1">
    <BaseButton
      icon-only
      variant="ghost"
      size="sm"
      label="编辑账号"
      @click.stop="emit('edit', account)"
    >
      <Pencil class="size-3.5 text-(--cp-info)" />
    </BaseButton>

    <BaseButton
      icon-only
      variant="ghost"
      size="sm"
      title="删除账号"
      :disabled="deleting"
      @click.stop="emit('delete', account)"
    >
      <Trash2 class="size-3.5 text-(--cp-danger)" />
    </BaseButton>

    <BasePopover placement="bottom-end" width="160px">
      <template #trigger="{ open }">
        <BaseButton icon-only variant="ghost" size="sm" title="更多操作" :active="open">
          <MoreHorizontal class="size-4" />
        </BaseButton>
      </template>

      <template #default="{ close }">
        <BaseButton
          variant="ghost"
          size="sm"
          class="h-8.5! w-full justify-start! gap-2! rounded-(--cp-input-radius-small)! px-3! text-left text-[13px] leading-none! font-emphasis! text-(--cp-text-primary)!"
          :loading="testing"
          :disabled="testing"
          @click.stop="(close(), emit('test', account))"
        >
          <template #icon>
            <Wifi class="size-3.5 text-(--cp-text-muted)" />
          </template>
          测试连接
        </BaseButton>
        <BaseButton
          variant="ghost"
          size="sm"
          class="h-8.5! w-full justify-start! gap-2! rounded-(--cp-input-radius-small)! px-3! text-left text-[13px] leading-none! font-emphasis! text-(--cp-text-primary)!"
          :loading="refreshing"
          :disabled="refreshing"
          @click.stop="(close(), emit('refresh', account.id))"
        >
          <template #icon>
            <RefreshCw class="size-3.5 text-(--cp-text-muted)" />
          </template>
          刷新令牌
        </BaseButton>
        <BaseButton
          variant="ghost"
          size="sm"
          class="h-8.5! w-full justify-start! gap-2! rounded-(--cp-input-radius-small)! px-3! text-left text-[13px] leading-none! font-emphasis! text-(--cp-text-primary)!"
          @click.stop="(close(), emit('reauthorize', account))"
        >
          <KeyRound class="size-3.5 text-(--cp-text-muted)" />
          重新授权
        </BaseButton>
        <BaseButton
          variant="ghost"
          size="sm"
          class="h-8.5! w-full justify-start! gap-2! rounded-(--cp-input-radius-small)! px-3! text-left text-[13px] leading-none! font-emphasis! text-(--cp-text-primary)!"
          :loading="recovering"
          :disabled="recovering"
          @click.stop="(close(), emit('recover', account.id))"
        >
          <template #icon>
            <RotateCcw class="size-3.5 text-(--cp-text-muted)" />
          </template>
          恢复状态
        </BaseButton>
      </template>
    </BasePopover>
  </div>
</template>
