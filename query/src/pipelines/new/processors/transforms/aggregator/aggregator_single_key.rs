// Copyright 2022 Datafuse Labs.
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

use std::borrow::BorrowMut;
use std::sync::Arc;

use bumpalo::Bump;
use bytes::BytesMut;
use common_datablocks::DataBlock;
use common_datavalues::ColumnRef;
use common_datavalues::DataSchemaRef;
use common_datavalues::DataType;
use common_datavalues::MutableColumn;
use common_datavalues::MutableStringColumn;
use common_datavalues::ScalarColumn;
use common_datavalues::ScalarColumnBuilder;
use common_datavalues::Series;
use common_datavalues::StringColumn;
use common_exception::ErrorCode;
use common_exception::Result;
use common_functions::aggregates::AggregateFunctionRef;
use common_functions::aggregates::StateAddr;

use crate::pipelines::new::processors::transforms::transform_aggregator::Aggregator;
use crate::pipelines::new::processors::AggregatorParams;

pub type FinalSingleStateAggregator = SingleStateAggregator<true>;
pub type PartialSingleStateAggregator = SingleStateAggregator<false>;

/// SELECT COUNT | SUM FROM table;
pub struct SingleStateAggregator<const FINAL: bool> {
    funcs: Vec<AggregateFunctionRef>,
    arg_names: Vec<Vec<String>>,
    schema: DataSchemaRef,
    _arena: Bump,
    places: Vec<StateAddr>,
    // used for deserialization only, so we can reuse it during the loop
    temp_places: Vec<StateAddr>,
    is_finished: bool,
    states_dropped: bool,
}

impl<const FINAL: bool> SingleStateAggregator<FINAL> {
    pub fn try_create(params: &Arc<AggregatorParams>) -> Result<Self> {
        assert!(!params.offsets_aggregate_states.is_empty());
        let arena = Bump::new();
        let layout = params
            .layout
            .ok_or_else(|| ErrorCode::LayoutError("layout shouldn't be None"))?;
        let get_places = || -> Vec<StateAddr> {
            let place: StateAddr = arena.alloc_layout(layout).into();
            params
                .aggregate_functions
                .iter()
                .enumerate()
                .map(|(idx, func)| {
                    let arg_place = place.next(params.offsets_aggregate_states[idx]);
                    func.init_state(arg_place);
                    arg_place
                })
                .collect()
        };

        let places = get_places();
        let temp_places = get_places();
        Ok(Self {
            _arena: arena,
            places,
            funcs: params.aggregate_functions.clone(),
            arg_names: params.aggregate_functions_arguments_name.clone(),
            schema: params.schema.clone(),
            temp_places,
            is_finished: false,
            states_dropped: false,
        })
    }

    fn drop_states(&mut self) {
        if !self.states_dropped {
            for (place, func) in self.places.iter().zip(self.funcs.iter()) {
                if func.need_manual_drop_state() {
                    unsafe { func.drop_state(*place) }
                }
            }

            for (place, func) in self.temp_places.iter().zip(self.funcs.iter()) {
                if func.need_manual_drop_state() {
                    unsafe { func.drop_state(*place) }
                }
            }

            self.states_dropped = true;
        }
    }
}

impl Aggregator for SingleStateAggregator<true> {
    const NAME: &'static str = "FinalSingleStateAggregator";

    fn consume(&mut self, block: DataBlock) -> Result<()> {
        for (index, func) in self.funcs.iter().enumerate() {
            let place = self.places[index];

            let binary_array = block.column(index);
            let binary_array: &StringColumn = Series::check_get(binary_array)?;

            let mut data = binary_array.get_data(0);

            let temp_addr = self.temp_places[index];
            func.deserialize(temp_addr, &mut data)?;
            func.merge(place, temp_addr)?;
        }

        Ok(())
    }

    fn generate(&mut self) -> Result<Option<DataBlock>> {
        if self.is_finished {
            return Ok(None);
        }

        self.is_finished = true;
        let mut aggr_values: Vec<Box<dyn MutableColumn>> = {
            let mut builders = vec![];
            for func in &self.funcs {
                let data_type = func.return_type()?;
                builders.push(data_type.create_mutable(1024));
            }
            builders
        };

        for (index, func) in self.funcs.iter().enumerate() {
            let place = self.places[index];
            let array: &mut dyn MutableColumn = aggr_values[index].borrow_mut();
            func.merge_result(place, array)?;
        }

        let mut columns: Vec<ColumnRef> = Vec::with_capacity(self.funcs.len());
        for mut array in aggr_values {
            columns.push(array.to_column());
        }

        Ok(Some(DataBlock::create(self.schema.clone(), columns)))
    }
}

impl Aggregator for SingleStateAggregator<false> {
    const NAME: &'static str = "PartialSingleStateAggregator";

    fn consume(&mut self, block: DataBlock) -> Result<()> {
        let rows = block.num_rows();
        for (idx, func) in self.funcs.iter().enumerate() {
            let mut arg_columns = vec![];
            for name in self.arg_names[idx].iter() {
                arg_columns.push(block.try_column_by_name(name)?.clone());
            }
            let place = self.places[idx];
            func.accumulate(place, &arg_columns, None, rows)?;
        }

        Ok(())
    }

    fn generate(&mut self) -> Result<Option<DataBlock>> {
        if self.is_finished {
            self.drop_states();
            return Ok(None);
        }

        self.is_finished = true;
        let mut columns = Vec::with_capacity(self.funcs.len());
        let mut bytes = BytesMut::new();

        for (idx, func) in self.funcs.iter().enumerate() {
            let place = self.places[idx];
            func.serialize(place, &mut bytes)?;
            let mut array_builder = MutableStringColumn::with_capacity(4);
            array_builder.append_value(&bytes[..]);
            bytes.clear();
            columns.push(array_builder.to_column());
        }

        // TODO: create with temp schema
        Ok(Some(DataBlock::create(self.schema.clone(), columns)))
    }
}

impl<const FINAL: bool> Drop for SingleStateAggregator<FINAL> {
    fn drop(&mut self) {
        self.drop_states();
    }
}
