"""Grades require executed wire/state evidence, not a manual success flag."""
import unittest
from types import SimpleNamespace
from scripts.proxy_gateway_native_fixture import grade_case


class NativeGatewayGradeTests(unittest.TestCase):
    def evidence(self):
        rpc=[{'type':'response','id':f'c{i}','success':True} for i in (1,2)] + [
            {'type':'agent_settled'} for _ in range(3)]
        fixture=SimpleNamespace(calls=[{'input':[], 'prompt_cache_key':'native'} for _ in range(4)] + [
            {'input':[{'content':'FIXTURE_PI_CHECKPOINT'}]} for _ in range(2)],
            summary_calls=[{},{}],errors=[])
        state={'history_rebases':2,'background_review_pending':False,'compactions':0}
        wire={'input':[{'content':'FIXTURE_PI_CHECKPOINT'}]}
        fixture.calls[-1]=wire
        return dict(exit_code=0,rpc=rpc,fixture=fixture,state=state,received=[wire]*6)

    def test_repeated_summary_requires_two_rebases_and_unchanged_checkpoint_wire(self):
        evidence=self.evidence()
        result=grade_case('compact-reset',**evidence)
        self.assertTrue(result.get('two_summary_continuation_verified'))
        self.assertTrue(result.get('checkpoint_wire_preserved'))
        for missing in ('summary','rebase','shadow','old-cohort','wire','missing-in','extra-in','missing-out','extra-out'):
            evidence=self.evidence()
            if missing=='summary': evidence['fixture'].summary_calls.pop()
            if missing=='rebase': evidence['state']['history_rebases']=1
            if missing=='shadow': evidence['state']['background_review_pending']=True
            if missing=='old-cohort': evidence['fixture'].calls[-1]['input'].append(
                {'type':'function_call_output','call_id':'old','output':'stale'})
            if missing=='wire': evidence['received'][-1]={'input':[]}
            if missing=='missing-in': evidence['received'].pop()
            if missing=='extra-in': evidence['received'].append(evidence['received'][-1])
            if missing=='missing-out': evidence['fixture'].calls.pop()
            if missing=='extra-out': evidence['fixture'].calls.append(evidence['fixture'].calls[-1])
            with self.subTest(missing=missing), self.assertRaises(AssertionError):
                grade_case('compact-reset',**evidence)

    def test_strict_case_requires_actual_409_without_provider_summary(self):
        evidence=self.evidence()
        evidence.update(exit_code=1,rpc=[{'type':'response','id':'c1','success':False,
            'error':'409 conflict'}])
        evidence['fixture'].calls=evidence['fixture'].calls[:2]
        evidence['fixture'].summary_calls=[]
        result=grade_case('compact-strict',**evidence)
        self.assertTrue(result.get('strict_409_verified'))
        evidence['rpc'][0]['error']='400 bad request'
        with self.assertRaises(AssertionError): grade_case('compact-strict',**evidence)


if __name__ == '__main__': unittest.main()
