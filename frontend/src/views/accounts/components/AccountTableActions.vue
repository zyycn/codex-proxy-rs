<script setup lang="ts">
import type { Account } from '@/api'

import { ZIconButton, ZMenuItem } from '@codex-proxy/ui'

import { Download, KeyRound, Pencil, Power, RefreshCw, RotateCcw, Trash2, Wifi } from '@lucide/vue'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import RowActionMenu from '@/components/RowActionMenu.vue'

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

const { t } = useI18n({ useScope: 'global' })

const credentialEligible = computed(() => props.account.authenticationKind === 'oauth')
</script>

<template>
  <div class="relative flex items-center justify-start gap-1">
    <ZIconButton
      size="small"
      :aria-label="t('accounts.editor.title')"
      @click.stop="emit('edit', account)"
    >
      <Pencil class="size-3.5 text-z-link" />
    </ZIconButton>

    <ZIconButton
      size="small"
      :aria-label="t('accounts.actions.deleteAccount')"
      :disabled="deleting"
      @click.stop="emit('delete', account)"
    >
      <Trash2 class="size-3.5 text-z-error" />
    </ZIconButton>

    <RowActionMenu :label="t('accounts.actions.moreActions')" menu-class="w-40 p-1.5">
      <ZMenuItem
        :loading="testing"
        :disabled="testing"
        @click.stop="emit('test', account)"
      >
        <template #icon>
          <Wifi class="size-3.5 text-z-text-quaternary" />
        </template>
        {{ t('accounts.connectionTest.title') }}
      </ZMenuItem>
      <ZMenuItem
        :loading="togglingScheduling"
        :disabled="togglingScheduling"
        @click.stop="emit('toggleScheduling', account)"
      >
        <template #icon>
          <Power class="size-3.5 text-z-text-quaternary" />
        </template>
        {{ account.enabled ? t('accounts.actions.disableScheduling') : t('accounts.actions.enableScheduling') }}
      </ZMenuItem>
      <ZMenuItem
        v-if="credentialEligible"
        :loading="refreshing"
        :disabled="refreshing"
        @click.stop="emit('refresh', account.id)"
      >
        <template #loading>
          <RefreshCw class="size-3.5 animate-spin text-z-text-quaternary motion-reduce:animate-none" />
        </template>
        <template #icon>
          <RefreshCw class="size-3.5 text-z-text-quaternary" />
        </template>
        {{ t('accounts.actions.refreshToken') }}
      </ZMenuItem>
      <ZMenuItem v-if="credentialEligible" @click.stop="emit('reauthorize', account)">
        <template #icon>
          <KeyRound class="size-3.5 text-z-text-quaternary" />
        </template>
        {{ t('accounts.actions.reauthorize') }}
      </ZMenuItem>
      <ZMenuItem
        v-if="account.provider === 'openai' && account.authenticationKind === 'oauth'"
        :loading="downloadingCatalog"
        :disabled="downloadingCatalog"
        @click.stop="emit('downloadModelCatalog', account)"
      >
        <template #loading>
          <RefreshCw class="size-3.5 animate-spin text-z-text-quaternary motion-reduce:animate-none" />
        </template>
        <template #icon>
          <Download class="size-3.5 text-z-text-quaternary" />
        </template>
        {{ t('accounts.actions.downloadModelCatalog') }}
      </ZMenuItem>
      <ZMenuItem
        :loading="recovering"
        :disabled="recovering"
        @click.stop="emit('recover', account.id)"
      >
        <template #icon>
          <RotateCcw class="size-3.5 text-z-text-quaternary" />
        </template>
        {{ t('accounts.actions.restoreState') }}
      </ZMenuItem>
    </RowActionMenu>
  </div>
</template>
