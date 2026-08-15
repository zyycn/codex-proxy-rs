<script setup lang="ts">
import BaseButton from '@/components/base/BaseButton.vue'
import BaseModal from '@/components/base/BaseModal.vue'

const open = defineModel<boolean>({ default: false })
</script>

<template>
  <BaseModal
    v-model="open"
    title="Cloudflare R2 接入指南"
    size="lg"
  >
    <div class="flex flex-col gap-4 text-[13px] leading-relaxed text-cp-secondary">
      <section class="flex flex-col gap-1.5">
        <h3 class="text-[14px] font-heavy text-cp-normal">
          1. 创建存储桶
        </h3>
        <p class="m-0">
          在 Cloudflare Dashboard 进入 R2，创建一个私有存储桶（例如
          <code class="rounded bg-cp-subtle px-1.5 py-0.5">codex-proxy-backups</code>）。
          桶必须保持私有，不要开放公共读取。
        </p>
      </section>

      <section class="flex flex-col gap-1.5">
        <h3 class="text-[14px] font-heavy text-cp-normal">
          2. 创建 API Token
        </h3>
        <p class="m-0">
          在「管理 API Token」创建对象存储专用 Token，只授予目标桶的对象
          <code class="rounded bg-cp-subtle px-1.5 py-0.5">对象读</code>、
          <code class="rounded bg-cp-subtle px-1.5 py-0.5">对象写</code>、
          <code class="rounded bg-cp-subtle px-1.5 py-0.5">对象查看</code> 与
          <code class="rounded bg-cp-subtle px-1.5 py-0.5">对象删除</code> 权限，
          不要授予整个账号权限。
        </p>
      </section>

      <section class="flex flex-col gap-1.5">
        <h3 class="text-[14px] font-heavy text-cp-normal">
          3. 填写连接参数
        </h3>
        <ul class="m-0 list-disc space-y-1 pl-5">
          <li>
            Endpoint：<code class="rounded bg-cp-subtle px-1.5 py-0.5">https://&#123;ACCOUNT_ID&#125;.r2.cloudflarestorage.com</code>
          </li>
          <li>Region：<code class="rounded bg-cp-subtle px-1.5 py-0.5">auto</code>（R2 固定值）</li>
          <li>Access Key ID / Secret Access Key：上一步生成的 Token</li>
          <li>Force Path Style：关闭</li>
        </ul>
      </section>

      <section class="flex flex-col gap-1.5">
        <h3 class="text-[14px] font-heavy text-cp-normal">
          4. 完成连接测试
        </h3>
        <p class="m-0">
          保存配置后点击「测试连接」，系统会写入一个探针对象并校验读写权限。测试通过后
          才能启用自动计划或手动创建备份。凭据只保存在服务端，不会写入浏览器。
        </p>
      </section>
    </div>

    <template #footer>
      <BaseButton variant="primary" @click="open = false">
        知道了
      </BaseButton>
    </template>
  </BaseModal>
</template>
