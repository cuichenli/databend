// Copyright 2021 Datafuse Labs.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::sync::Arc;

use common_exception::Result;
use common_planners::CopyPlan;
use common_planners::PlanNode;
use common_streams::DataBlockStream;
use common_streams::ProgressStream;
use common_streams::SendableDataBlockStream;
use common_tracing::tracing;
use futures::TryStreamExt;

use crate::interpreters::stream::ProcessorExecutorStream;
use crate::interpreters::Interpreter;
use crate::interpreters::InterpreterPtr;
use crate::pipelines::new::executor::PipelinePullingExecutor;
use crate::pipelines::new::QueryPipelineBuilder;
use crate::sessions::QueryContext;

pub struct CopyInterpreter {
    ctx: Arc<QueryContext>,
    plan: CopyPlan,
}

impl CopyInterpreter {
    pub fn try_create(ctx: Arc<QueryContext>, plan: CopyPlan) -> Result<InterpreterPtr> {
        Ok(Arc::new(CopyInterpreter { ctx, plan }))
    }

    // Read a file and commit it to the table.
    // Progress:
    // 1. Build a select pipeline
    // 2. Execute the pipeline and get the stream
    // 3. Read from the stream and write to the table.
    // Note:
    //  We parse the `s3://` to ReadSourcePlan instead of to a SELECT plan is:
    //  COPY should deal with the file one by one and do some error handler by the OnError strategy.
    async fn copy_one_file_to_table(&self, _file_name: Option<String>) -> Result<()> {
        let ctx = self.ctx.clone();
        let settings = self.ctx.get_settings();

        let read_source_plan = self.plan.from.clone();
        let from_plan = common_planners::SelectPlan {
            input: Arc::new(PlanNode::ReadSource(read_source_plan)),
        };

        let pipeline_builder = QueryPipelineBuilder::create(ctx.clone());
        let mut pipeline = pipeline_builder.finalize(&from_plan)?;
        pipeline.set_max_threads(settings.get_max_threads()? as usize);

        let executor = PipelinePullingExecutor::try_create(pipeline)?;
        let source_stream = Box::pin(ProcessorExecutorStream::create(executor)?);
        let progress_stream = Box::pin(ProgressStream::try_create(
            source_stream,
            ctx.get_scan_progress(),
        )?);

        let table = ctx
            .get_table(&self.plan.db_name, &self.plan.tbl_name)
            .await?;
        let operations = table
            .append_data(ctx.clone(), progress_stream)
            .await?
            .try_collect()
            .await?;

        // Commit.
        table
            .commit_insertion(ctx.clone(), operations, false)
            .await?;

        Ok(())
    }
}

#[async_trait::async_trait]
impl Interpreter for CopyInterpreter {
    fn name(&self) -> &str {
        "CopyInterpreter"
    }

    async fn execute(
        &self,
        mut _input_stream: Option<SendableDataBlockStream>,
    ) -> Result<SendableDataBlockStream> {
        tracing::info!("Plan:{:?}", self.plan);

        let files = self.plan.files.clone();

        if files.is_empty() {
            self.copy_one_file_to_table(None).await?;
        } else {
            for file in files {
                self.copy_one_file_to_table(Some(file)).await?;
            }
        }

        Ok(Box::pin(DataBlockStream::create(
            self.plan.schema(),
            None,
            vec![],
        )))
    }
}
