"""Exercise new exact IDs through shared Pi projection and receipt pricing."""
import unittest
import tempfile
import json
from pathlib import Path
from scripts import model_catalog, proxy_trial, swebench_smoke

class GPT62Catalog(unittest.TestCase):
 def test_native_projection_and_tier_rates(self):
  for model,input_rate,cached,write,output in [('gpt-6.2-sol',2,.1,2.5,10),('gpt-6.2-astra',8,.4,10,40)]:
   with self.subTest(model=model),tempfile.TemporaryDirectory() as directory:
    proxy_trial.client_configuration(Path(directory),client='pi',base_url='http://127.0.0.1:1/v1',model=model,session_id=None)
    row=json.loads((Path(directory)/'pi/models.json').read_text())['providers']['carry-trial']['models'][0]
    self.assertEqual(row['id'],model);self.assertEqual(row['contextWindow'],1050000);self.assertEqual(row['maxTokens'],128000)
    self.assertEqual(row['cost'],{'input':input_rate,'cacheRead':cached,'cacheWrite':write,'output':output})
    price=swebench_smoke.pricing_for_model(model)
    self.assertEqual(price['long_context_threshold'],272000)
    self.assertEqual(price['input'],input_rate)
 def test_unknown_variants_not_guessed(self):
  for model in ['gpt-6.2-sol-extra','gpt-6.2-astra-900k','gpt-6.2']:
   self.assertIsNone(model_catalog.model_spec(model));self.assertIsNone(swebench_smoke.pricing_for_model(model))

if __name__=='__main__':unittest.main()
