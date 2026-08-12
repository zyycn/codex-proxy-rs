<script setup lang="ts">
import type { AccountRow } from '../constants'
import { KeyRound, MoreHorizontal, Power, RefreshCw, Trash2, Wifi } from '@lucide/vue'

import { computed } from 'vue'
import BaseButton from '@/components/base/BaseButton.vue'
import BasePopover from '@/components/base/BasePopover.vue'

const props = defineProps<{
  account: AccountRow
  deleting: boolean
  refreshing: boolean
  testing: boolean
  updatingStatus: boolean
  scheduleLabel: string
}>()

const emit = defineEmits<{
  delete: [account: AccountRow]
  test: [account: AccountRow]
  refresh: [accountId: string]
  reauthorize: [account: AccountRow]
  toggleSchedule: [account: AccountRow]
}>()

const canRefreshToken = computed(
  () =>
    props.account.hasRefreshToken
    && (props.account.status === 'normal' || props.account.status === 'quota_exhausted'),
)
const canReauthorize = computed(() => props.account.provider === 'openai')
</script>

<template>
  <div class="relative flex items-center justify-start gap-1">
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
          <Wifi class="size-3.5 text-(--cp-text-muted)" />
          测试连接
        </BaseButton>
        <BaseButton
          v-if="canRefreshToken"
          variant="ghost"
          size="sm"
          class="h-8.5! w-full justify-start! gap-2! rounded-(--cp-input-radius-small)! px-3! text-left text-[13px] leading-none! font-emphasis! text-(--cp-text-primary)!"
          :loading="refreshing"
          :disabled="refreshing"
          @click.stop="(close(), emit('refresh', account.id))"
        >
          <RefreshCw class="size-3.5 text-(--cp-text-muted)" />
          刷新令牌
        </BaseButton>
        <BaseButton
          v-if="canReauthorize"
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
          :loading="updatingStatus"
          type="button"
          :disabled="updatingStatus"
          @click.stop="(close(), emit('toggleSchedule', account))"
        >
          <Power class="size-3.5 text-(--cp-text-muted)" />
          {{ scheduleLabel }}
        </BaseButton>
      </template>
    </BasePopover>
  </div>
</template>
