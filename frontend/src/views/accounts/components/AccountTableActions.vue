<script setup lang="ts">
import type { Account } from '@/api'

import { BaseIconButton, BaseMenuItem, BasePopover } from '@codex-proxy/ui'

import { Download, KeyRound, MoreHorizontal, Pencil, Power, RefreshCw, RotateCcw, Trash2, Wifi } from '@lucide/vue'
import { computed } from 'vue'

const props = defineProps<{
  account: Account
  deleting: boolean
  downloadingCatalog: boolean
  recovering: boolean
  refreshing: boolean
  testing: boolean
  togglingScheduling: boolean
}>()
const emit = defineEmits<{
  edit: [account: Account]
  delete: [account: Account]
  recover: [accountId: string]
  test: [account: Account]
  refresh: [accountId: string]
  reauthorize: [account: Account]
  downloadModelCatalog: [account: Account]
  toggleScheduling: [account: Account]
}>()

const credentialEligible = computed(() => props.account.authenticationKind === 'oauth')
</script>

<template>
  <div class="relative flex items-center justify-start gap-1">
    <BaseIconButton
      variant="ghost"
      size="sm"
      label="编辑账号"
      @click.stop="emit('edit', account)"
    >
      <Pencil class="size-3.5 text-cp-link" />
    </BaseIconButton>

    <BaseIconButton
      variant="ghost"
      size="sm"
      label="删除账号"
      :disabled="deleting"
      @click.stop="emit('delete', account)"
    >
      <Trash2 class="size-3.5 text-cp-error" />
    </BaseIconButton>

    <BasePopover placement="bottom-end">
      <template #trigger="{ open }">
        <BaseIconButton variant="ghost" size="sm" label="更多操作" :pressed="open">
          <MoreHorizontal class="size-4" />
        </BaseIconButton>
      </template>

      <template #default="{ close }">
        <div class="w-40 p-1.5">
          <BaseMenuItem
            :loading="testing"
            :disabled="testing"
            @click.stop="(close(), emit('test', account))"
          >
            <template #icon>
              <Wifi class="size-3.5 text-cp-text-quaternary" />
            </template>
            测试连接
          </BaseMenuItem>
          <BaseMenuItem
            :loading="togglingScheduling"
            :disabled="togglingScheduling"
            @click.stop="(close(), emit('toggleScheduling', account))"
          >
            <template #icon>
              <Power class="size-3.5 text-cp-text-quaternary" />
            </template>
            {{ account.enabled ? '停用调度' : '启用调度' }}
          </BaseMenuItem>
          <BaseMenuItem
            v-if="credentialEligible"
            :loading="refreshing"
            :disabled="refreshing"
            @click.stop="(close(), emit('refresh', account.id))"
          >
            <template #loading>
              <RefreshCw class="size-3.5 animate-spin text-cp-text-quaternary motion-reduce:animate-none" />
            </template>
            <template #icon>
              <RefreshCw class="size-3.5 text-cp-text-quaternary" />
            </template>
            刷新令牌
          </BaseMenuItem>
          <BaseMenuItem v-if="credentialEligible" @click.stop="(close(), emit('reauthorize', account))">
            <template #icon>
              <KeyRound class="size-3.5 text-cp-text-quaternary" />
            </template>
            重新授权
          </BaseMenuItem>
          <BaseMenuItem
            v-if="account.provider === 'openai' && account.authenticationKind === 'oauth'"
            :loading="downloadingCatalog"
            :disabled="downloadingCatalog"
            @click.stop="(close(), emit('downloadModelCatalog', account))"
          >
            <template #loading>
              <RefreshCw class="size-3.5 animate-spin text-cp-text-quaternary motion-reduce:animate-none" />
            </template>
            <template #icon>
              <Download class="size-3.5 text-cp-text-quaternary" />
            </template>
            下载模型目录
          </BaseMenuItem>
          <BaseMenuItem
            :loading="recovering"
            :disabled="recovering"
            @click.stop="(close(), emit('recover', account.id))"
          >
            <template #icon>
              <RotateCcw class="size-3.5 text-cp-text-quaternary" />
            </template>
            恢复状态
          </BaseMenuItem>
        </div>
      </template>
    </BasePopover>
  </div>
</template>
