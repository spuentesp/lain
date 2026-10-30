"""Regenerate runtime.onnx with `python -m pip install onnx` available.

This small, untrained graph checks session inputs, dynamic batching and output
shapes without downloading a production embedding model.
"""
from pathlib import Path
import onnx
from onnx import TensorProto, helper

inputs = [helper.make_tensor_value_info(name, TensorProto.INT64, ["batch", "sequence"])
          for name in ("input_ids", "attention_mask", "token_type_ids")]
nodes = [
    helper.make_node("Cast", ["input_ids"], ["ids"], to=TensorProto.FLOAT),
    helper.make_node("Cast", ["attention_mask"], ["mask"], to=TensorProto.FLOAT),
    helper.make_node("Unsqueeze", ["ids", "axis"], ["ids3"]),
    helper.make_node("Unsqueeze", ["mask", "axis"], ["mask3"]),
    helper.make_node("Concat", ["ids3", "mask3"], ["last_hidden_state"], axis=2),
    helper.make_node("ReduceSum", ["last_hidden_state", "reduce_axes"], ["logits"], keepdims=0),
]
graph = helper.make_graph(nodes, "lain-runtime-fixture", inputs, [
    helper.make_tensor_value_info("last_hidden_state", TensorProto.FLOAT, ["batch", "sequence", 2]),
    helper.make_tensor_value_info("logits", TensorProto.FLOAT, ["batch"]),
], initializer=[
    helper.make_tensor("axis", TensorProto.INT64, [1], [2]),
    helper.make_tensor("reduce_axes", TensorProto.INT64, [2], [1, 2]),
])
model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 13)], ir_version=8)
onnx.checker.check_model(model)
onnx.save(model, Path(__file__).with_name("runtime.onnx"))
