<script setup lang="ts">
import type { AccountRow } from '../../constants'
import { RefreshCw, TriangleAlert } from '@lucide/vue'
import { toRef } from 'vue'

import BaseButton from '@/components/base/BaseButton.vue'
import BaseEmpty from '@/components/base/BaseEmpty.vue'
import BaseModal from '@/components/base/BaseModal/index.vue'
import { useAccountProfileStatistics } from '../../composables/useAccountProfileStatistics'
import AccountProfileActivityInsights from './ActivityInsights.vue'
import AccountProfileHero from './ProfileHero.vue'
import AccountProfileSkeleton from './Skeleton.vue'
import AccountProfileTokenActivity from './TokenActivity.vue'

const props = defineProps<{
  account: AccountRow
}>()
const open = defineModel<boolean>({ required: true })
const accountId = toRef(() => props.account.id)
const { profile, loading, error, load } = useAccountProfileStatistics(accountId, open)
</script>

<template>
  <BaseModal
    v-model="open"
    title="个人资料"
    description="来自 Codex 官方个人资料的累计活动与使用洞察"
    size="xl"
  >
    <AccountProfileSkeleton v-if="loading && !profile" />

    <BaseEmpty
      v-else-if="error && !profile"
      title="个人资料加载失败"
      :description="error"
      :icon="TriangleAlert"
      surface="none"
    >
      <template #action>
        <BaseButton size="sm" :loading="loading" @click="load(true)">
          重试
        </BaseButton>
      </template>
    </BaseEmpty>

    <div v-else-if="profile" class="flex min-h-0 flex-col gap-8 pb-2 sm:gap-10">
      <AccountProfileHero :account="account" :profile="profile" />

      <BaseEmpty
        v-if="profile.hasStatsError"
        title="个人统计暂不可用"
        description="官方个人资料已加载，但本次没有返回统计数据。"
        :icon="TriangleAlert"
        surface="none"
      />

      <template v-else>
        <AccountProfileTokenActivity :daily-usage="profile.dailyUsage" />
        <AccountProfileActivityInsights :insights="profile.activityInsights" />
      </template>
    </div>

    <template #footer>
      <span v-if="error && profile" class="mr-auto self-center text-cp-xs font-semibold text-cp-error-text">
        {{ error }}
      </span>
      <BaseButton variant="ghost" @click="open = false">
        关闭
      </BaseButton>
      <BaseButton :loading="loading" @click="load(true)">
        <template #loading>
          <RefreshCw class="size-4 animate-spin motion-reduce:animate-none" />
        </template>
        <template #icon>
          <RefreshCw class="size-4" />
        </template>
        刷新资料
      </BaseButton>
    </template>
  </BaseModal>
</template>
