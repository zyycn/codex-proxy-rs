<script setup lang="ts">
import type { AccountRow } from '../quota'
import { KeyRound, LoaderCircle, MoreHorizontal, Power, RefreshCw, Trash2, Wifi } from '@lucide/vue'

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
    && (props.account.status === 'active' || props.account.status === 'quota_exhausted'),
)
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
        <button
          type="button"
          class="flex h-8.5 w-full items-center gap-2 rounded-(--cp-input-radius-small) border-0 bg-transparent px-3 text-left text-[13px] leading-none font-[650] text-(--cp-text-primary) transition-colors hover:bg-(--cp-default-bg-hover) disabled:cursor-not-allowed disabled:text-(--cp-disabled-text)"
          :disabled="testing"
          @click.stop="(close(), emit('test', account))"
        >
          <LoaderCircle v-if="testing" class="size-3.5 animate-spin text-(--cp-text-muted)" />
          <Wifi v-else class="size-3.5 text-(--cp-text-muted)" />
          测试连接
        </button>
        <button
          v-if="canRefreshToken"
          type="button"
          class="flex h-8.5 w-full items-center gap-2 rounded-(--cp-input-radius-small) border-0 bg-transparent px-3 text-left text-[13px] leading-none font-[650] text-(--cp-text-primary) transition-colors hover:bg-(--cp-default-bg-hover) disabled:cursor-not-allowed disabled:text-(--cp-disabled-text)"
          :disabled="refreshing"
          @click.stop="(close(), emit('refresh', account.id))"
        >
          <RefreshCw
            class="size-3.5 text-(--cp-text-muted)"
            :class="refreshing ? 'animate-spin' : undefined"
          />
          刷新 token
        </button>
        <button
          type="button"
          class="flex h-8.5 w-full items-center gap-2 rounded-(--cp-input-radius-small) border-0 bg-transparent px-3 text-left text-[13px] leading-none font-[650] text-(--cp-text-primary) transition-colors hover:bg-(--cp-default-bg-hover)"
          @click.stop="(close(), emit('reauthorize', account))"
        >
          <KeyRound class="size-3.5 text-(--cp-text-muted)" />
          重新授权
        </button>
        <button
          type="button"
          class="flex h-8.5 w-full items-center gap-2 rounded-(--cp-input-radius-small) border-0 bg-transparent px-3 text-left text-[13px] leading-none font-[650] text-(--cp-text-primary) transition-colors hover:bg-(--cp-default-bg-hover) disabled:cursor-not-allowed disabled:text-(--cp-disabled-text)"
          :disabled="updatingStatus"
          @click.stop="(close(), emit('toggleSchedule', account))"
        >
          <LoaderCircle
            v-if="updatingStatus"
            class="size-3.5 animate-spin text-(--cp-text-muted)"
          />
          <Power v-else class="size-3.5 text-(--cp-text-muted)" />
          {{ scheduleLabel }}
        </button>
      </template>
    </BasePopover>
  </div>
</template>
